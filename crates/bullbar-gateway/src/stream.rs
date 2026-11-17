//! Streamed replies (`stream: true`): server-sent events in the chat API's chunk shape. Bullbar's
//! own words go out as chunks of their own; a model's answer is relayed chunk by chunk with the
//! model name Bullbar was asked for, and the footer goes in before the chunk that ends the answer.
//!
//! Credential placeholders pass through as they come: they are never put back (ADR 0009, R1).
//! Surrogates for personal details are put back as the answer streams (ADR 0020, point 8): text that
//! might be the start of one waits for the next chunk. A model's stream that ends without saying it is
//! finished, or breaks, is marked as cut off; nothing is retried anywhere else (rule 4).

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;

use async_openai::error::OpenAIError;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::fields;
use crate::reply::{FOOTER_LINE_BREAK, FOOTER_SEPARATOR, MARKER};
use crate::scrub::{Restorer, Streaming};

/// A model's answer as a stream of chunks.
pub type Chunks = Pin<Box<dyn Stream<Item = Result<Value, OpenAIError>> + Send>>;

/// How often a comment line goes out while nothing else does, so clients and proxies keep the
/// connection open while a question waits.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Writes one streamed reply. Every send says whether the client is still there; once it isn't,
/// the writer's owner stops (and drops the model's stream, closing that connection too).
pub struct Writer {
    tx: mpsc::Sender<Event>,
    id: String,
    model: String,
    created: u64,
}

/// A writer and the response that carries what it writes.
pub fn channel(model: &str, created: u64) -> (Writer, Response) {
    let (tx, rx) = mpsc::channel::<Event>(32);
    let events = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|e| (Ok::<_, Infallible>(e), rx))
    });
    let response = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    let writer = Writer {
        tx,
        id: format!("bullbar-{created}"),
        model: model.to_owned(),
        created,
    };
    (writer, response)
}

impl Writer {
    /// The client has gone away.
    pub fn is_gone(&self) -> bool {
        self.tx.is_closed()
    }

    /// Waits until the client goes away.
    pub async fn closed(&self) {
        self.tx.closed().await;
    }

    async fn send(&self, data: String) -> bool {
        self.tx.send(Event::default().data(data)).await.is_ok()
    }

    async fn chunk(&self, chunk: &Value) -> bool {
        self.send(chunk.to_string()).await
    }

    /// Text in Bullbar's own chunk.
    pub async fn text(&self, text: &str) -> bool {
        self.chunk(&self.own_chunk(json!({ "role": "assistant", "content": text }), None))
            .await
    }

    /// Ends the reply: a chunk saying it's finished, then `[DONE]`.
    pub async fn finish(&self) -> bool {
        self.chunk(&self.own_chunk(json!({}), Some("stop"))).await && self.done().await
    }

    async fn done(&self) -> bool {
        self.send("[DONE]".to_owned()).await
    }

    fn own_chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        })
    }

    /// A whole reply Bullbar writes itself, marked.
    pub async fn own(&self, text: &str) -> bool {
        self.text(&format!("{MARKER} {text}")).await && self.finish().await
    }

    /// Relays a model's answer: each chunk with the model name renamed, only the answer's own fields
    /// kept and its text restored, then
    /// for each choice that ends, what was held back and the footer (if any, with the restorer's
    /// report) before the chunk that ends it. Returns whether the answer came whole.
    pub async fn relay(
        &self,
        mut chunks: Chunks,
        footer: Option<&str>,
        who: &str,
        restorer: &Restorer,
    ) -> Relayed {
        let mut finished = BTreeSet::new();
        let mut last: Option<Value> = None;
        let mut streams: BTreeMap<u64, Streaming<'_>> = BTreeMap::new();
        loop {
            let chunk = match chunks.next().await {
                None => break,
                Some(Ok(chunk)) => chunk,
                // The model's own words about the failure aren't passed on: they can quote the
                // request.
                Some(Err(_)) => return self.cut_off(last, footer, who, streams).await,
            };
            // Only answer chunks are relayed; anything else (an error object mid-stream) ends it.
            if chunk.get("choices").and_then(Value::as_array).is_none() {
                return self.cut_off(last, footer, who, streams).await;
            }
            let mut chunk = chunk.clone();
            chunk["model"] = Value::String(self.model.clone());
            let mut ending = Vec::new();
            if let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) {
                for choice in choices {
                    let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let stream = streams.entry(index).or_insert_with(|| restorer.stream());
                    if let Some(delta) = choice.get_mut("delta") {
                        fields::answer_only(delta);
                    }
                    if let Some(Value::String(text)) = choice.pointer_mut("/delta/content") {
                        *text = stream.push(text);
                    }
                    if choice.get("finish_reason").is_some_and(|f| !f.is_null()) {
                        ending.push(index);
                    }
                }
            }
            for &index in &ending {
                let (held, report) = streams
                    .get_mut(&index)
                    .map(Streaming::finish)
                    .unwrap_or_default();
                let mut text = held;
                if let Some(footer) = with_report(footer, &report) {
                    text.push_str(FOOTER_SEPARATOR);
                    text.push_str(&footer);
                }
                if !text.is_empty() && !self.chunk(&content_like(&chunk, index, &text)).await {
                    return Relayed::Gone;
                }
            }
            finished.extend(ending);
            if !self.chunk(&chunk).await {
                return Relayed::Gone;
            }
            last = Some(chunk);
        }
        if finished.is_empty() {
            return self.cut_off(last, footer, who, streams).await;
        }
        if self.done().await {
            Relayed::Whole
        } else {
            Relayed::Gone
        }
    }

    /// Marks an answer that stopped before it was finished, then ends the reply. What was held
    /// back goes out restored first.
    async fn cut_off(
        &self,
        last: Option<Value>,
        footer: Option<&str>,
        who: &str,
        mut streams: BTreeMap<u64, Streaming<'_>>,
    ) -> Relayed {
        let (held, report) = streams
            .get_mut(&0)
            .map(Streaming::finish)
            .unwrap_or_default();
        let mut text = format!(
            "{held}{FOOTER_SEPARATOR}the answer was cut off: {who} stopped before it was finished. \
             Nothing was sent anywhere else."
        );
        if let Some(footer) = with_report(footer, &report) {
            text.push_str(FOOTER_LINE_BREAK);
            text.push_str(&footer);
        }
        let template = last.unwrap_or_else(|| self.own_chunk(json!({}), None));
        if self.chunk(&content_like(&template, 0, &text)).await && self.finish().await {
            Relayed::CutOff
        } else {
            Relayed::Gone
        }
    }
}

/// The footer with the restorer's report lines after it; `None` when both are empty.
pub fn with_report(footer: Option<&str>, report: &[String]) -> Option<String> {
    let lines: Vec<&str> = footer
        .into_iter()
        .chain(report.iter().map(String::as_str))
        .collect();
    (!lines.is_empty()).then(|| lines.join(FOOTER_LINE_BREAK))
}

/// How a relayed answer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relayed {
    /// Every choice finished.
    Whole,
    /// The model's stream broke or ended early; marked as cut off.
    CutOff,
    /// The client went away.
    Gone,
}

/// A chunk shaped like `template` (its id, created, model) carrying `text` for one choice.
fn content_like(template: &Value, index: u64, text: &str) -> Value {
    let mut chunk = template.clone();
    if let Some(object) = chunk.as_object_mut() {
        object.remove("usage");
    }
    chunk["choices"] =
        json!([{ "index": index, "delta": { "content": text }, "finish_reason": null }]);
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_chunk_keeps_the_template_but_not_its_usage() {
        let template = json!({ "id": "x", "model": "bullbar", "usage": { "total_tokens": 3 },
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] });
        let chunk = content_like(&template, 0, "hi");
        assert_eq!(chunk["id"], "x");
        assert!(chunk.get("usage").is_none());
        assert_eq!(chunk["choices"][0]["delta"]["content"], "hi");
        assert!(chunk["choices"][0]["finish_reason"].is_null());
    }
}
