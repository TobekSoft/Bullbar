//! Only the chat API's own fields leave for a cloud account (rule 3; ADR 0017, settled 2026-11-16).
//! Chat clients add their own: settings for a local runner (`top_k`), identifiers (`user`), requests
//! to keep the conversation (`store`), and the provider's earlier reasoning handed back on the
//! assistant's turn, which a provider may refuse with a 400 (found in the chunk 3 hand check). A
//! field Bullbar doesn't know is a field it hasn't judged, so it stays home and is named.
//!
//! Inside a message's content parts and tool calls, the shapes are the API's own and are passed as
//! they are. The local runner is ours and gets the request as written.

use serde_json::Value;

/// Top-level fields of a chat request that may leave.
const REQUEST: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "max_tokens",
    "max_completion_tokens",
    "n",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "seed",
    "response_format",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning_effort",
];

/// Fields of a message that may leave. Not `name`: a participant's name.
const MESSAGE: &[&str] = &["role", "content", "tool_calls", "tool_call_id"];

/// Fields of an answer's message, or of a streamed chunk's delta, that reach the client. Not the
/// model's reasoning (`reasoning`, `reasoning_content`): it is in the model's words, with the
/// surrogates and placeholders a provider saw, and chat clients send it back on the next turn. A
/// field not listed is one Bullbar hasn't judged, as on the way out; a trace still shows it
/// (decided 2026-11-17, ADR 0017).
const ANSWER: &[&str] = &["role", "content", "tool_calls", "refusal"];

/// Removes every field of an answer's message (or delta) not in [`ANSWER`].
pub fn answer_only(message: &mut Value) {
    if let Some(fields) = message.as_object_mut() {
        fields.retain(|name, _| ANSWER.contains(&name.as_str()));
    }
}

/// Removes every field not in the lists above; returns what was removed, by name, sorted and once
/// each (`messages.<name>` for a message's). A name that isn't a plain identifier shows as
/// "(a field)": the footer can travel on in a later turn.
pub fn known_only(body: &mut Value) -> Vec<String> {
    let mut dropped = Vec::new();
    let Some(fields) = body.as_object_mut() else {
        return dropped;
    };
    fields.retain(|name, _| keep(name, REQUEST, "", &mut dropped));
    if let Some(Value::Array(messages)) = fields.get_mut("messages") {
        for message in messages.iter_mut().filter_map(Value::as_object_mut) {
            message.retain(|name, _| keep(name, MESSAGE, "messages.", &mut dropped));
        }
    }
    dropped.sort();
    dropped.dedup();
    dropped
}

fn keep(name: &str, known: &[&str], prefix: &str, dropped: &mut Vec<String>) -> bool {
    if known.contains(&name) {
        return true;
    }
    let plain = name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    dropped.push(if plain {
        format!("{prefix}{name}")
    } else {
        format!("{prefix}(a field)")
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_answer_keeps_only_its_own_fields() {
        let mut message = json!({ "role": "assistant", "content": "42", "reasoning": "r",
                                  "reasoning_content": "r", "channel": "final" });
        answer_only(&mut message);
        assert_eq!(message, json!({ "role": "assistant", "content": "42" }));
    }

    #[test]
    fn unknown_fields_are_removed_and_named_never_shown_raw() {
        let mut body = json!({
            "model": "m", "top_k": 5, "Imogen Fakeworth": 1, "user": "u",
            "messages": [{ "role": "user", "content": "hi", "name": "Imogen" },
                         { "role": "assistant", "content": [{ "type": "text", "text": "x", "extra": 1 }],
                           "reasoning": "r" }]
        });
        let dropped = known_only(&mut body);
        assert_eq!(
            dropped,
            [
                "(a field)",
                "messages.name",
                "messages.reasoning",
                "top_k",
                "user"
            ]
        );
        assert_eq!(
            body,
            json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" },
                { "role": "assistant", "content": [{ "type": "text", "text": "x", "extra": 1 }] }] })
        );
    }
}
