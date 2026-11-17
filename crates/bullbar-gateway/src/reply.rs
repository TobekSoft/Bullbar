//! What Bullbar itself says in a chat: its own replies, and the footer naming where an answer came
//! from. Both carry a marker, so when the client sends the conversation back, Bullbar's words are
//! dropped before anything goes to a model (two transcripts: the model never sees Bullbar's side).

use serde_json::{Value, json};

/// Starts every message Bullbar writes itself.
pub const MARKER: &str = "[Bullbar]";

/// Separates an answer from Bullbar's footer.
pub const FOOTER_SEPARATOR: &str = "\n\n[Bullbar] ";

/// Between a footer's lines: two spaces before the line break, Markdown's way of keeping it, so a
/// client that renders Markdown doesn't run the lines together. Never a blank line: that would end
/// the footer block (see `strip_own`).
pub const FOOTER_LINE_BREAK: &str = "  \n";

/// In a streamed reply that began with Bullbar's question, separates the question from the model's
/// answer that follows once the user said yes. What comes after it is the model's.
pub const ANSWER_SEPARATOR: &str = "\n\n[Bullbar] answered; the model's answer follows.\n\n";

/// A chat completion written by Bullbar rather than a model, in the chat API's shape, so any client
/// shows it as an ordinary reply.
pub fn own(model: &str, text: &str, created: u64) -> Value {
    json!({
        "id": format!("bullbar-{created}"),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": format!("{MARKER} {text}") },
            "finish_reason": "stop",
        }],
    })
}

/// Drops Bullbar's words from a conversation a client sent back: messages Bullbar wrote are
/// removed, and footers are cut from wherever they are: the end of the answers they were added to,
/// or inside any text a client copied a transcript into (a title request). A user message repeated right
/// after a removed Bullbar message is the same message resent (after a notice, say), and is kept
/// once. A streamed reply that began with Bullbar's question keeps only the model's answer after it.
pub fn strip_own(messages: &mut Vec<Value>) {
    let mut kept: Vec<Value> = Vec::with_capacity(messages.len());
    let mut removed_since_user = false;
    for mut message in messages.drain(..) {
        if is_own(&message)
            && let Some(Value::String(content)) = message.get_mut("content")
            && let Some(at) = content.find(ANSWER_SEPARATOR)
        {
            *content = content.split_off(at + ANSWER_SEPARATOR.len());
        }
        if is_own(&message) {
            removed_since_user = true;
            continue;
        }
        let is_user = message.get("role").and_then(Value::as_str) == Some("user");
        if is_user && removed_since_user && kept.last() == Some(&message) {
            continue;
        }
        if is_user {
            removed_since_user = false;
        }
        kept.push(message);
    }
    for message in &mut kept {
        match message.get_mut("content") {
            Some(Value::String(content)) => *content = without_footers(content),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(Value::String(text)) = part.get_mut("text") {
                        *text = without_footers(text);
                    }
                }
            }
            _ => {}
        }
    }
    *messages = kept;
}

/// `text` with every footer block taken out: from a footer separator to the next blank line, or to
/// the end. Footer lines are joined by single line breaks, so a blank line ends the block. Footers
/// name originals for the user ("not in the answer: …"), so none may travel on.
fn without_footers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(FOOTER_SEPARATOR) {
        out.push_str(&rest[..at]);
        let block = &rest[at + FOOTER_SEPARATOR.len()..];
        rest = match block.find("\n\n") {
            Some(end) => &block[end..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn is_own(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|c| c.starts_with(MARKER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_replies_are_marked_chat_completions() {
        let r = own("bullbar", "refused", 7);
        assert_eq!(r["object"], "chat.completion");
        assert_eq!(r["choices"][0]["message"]["content"], "[Bullbar] refused");
    }

    #[test]
    fn bullbar_words_are_dropped_from_history() {
        let answer = json!({ "choices": [{ "message": { "role": "assistant",
            "content": format!("42{FOOTER_SEPARATOR}answered at local / small") } }] });
        let mut messages = vec![
            json!({ "role": "user", "content": "first" }),
            json!({ "role": "assistant", "content": "[Bullbar] refused: ..." }),
            json!({ "role": "user", "content": "second" }),
            answer["choices"][0]["message"].clone(),
            json!({ "role": "user", "content": "third" }),
        ];
        strip_own(&mut messages);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["content"], "42");
    }

    #[test]
    fn a_message_resent_after_a_notice_counts_once() {
        let q = json!({ "role": "user", "content": "hard question" });
        let mut messages = vec![
            q.clone(),
            json!({ "role": "assistant", "content": "[Bullbar] waiting for your answer" }),
            q.clone(),
        ];
        strip_own(&mut messages);
        assert_eq!(messages, vec![q.clone()]);
        // Asked twice on purpose, with nothing of Bullbar's between: both stay.
        let mut twice = vec![q.clone(), q.clone()];
        strip_own(&mut twice);
        assert_eq!(twice.len(), 2);
    }

    #[test]
    fn a_streamed_answer_after_a_question_keeps_only_the_answer() {
        let streamed = format!(
            "[Bullbar] settle for a mid model?{ANSWER_SEPARATOR}42{FOOTER_SEPARATOR}answered by local"
        );
        let q = json!({ "role": "user", "content": "hard question" });
        let mut messages = vec![
            q.clone(),
            json!({ "role": "assistant", "content": streamed }),
            json!({ "role": "user", "content": "and seven times six?" }),
        ];
        strip_own(&mut messages);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"], "42");
    }

    /// A client that copies the transcript into a new request (a title request, found in the chunk
    /// 3 hand check) carries Bullbar's footers inside a user message. They name the originals, so
    /// they go wherever they are: any message, any text part.
    #[test]
    fn footers_inside_any_text_are_dropped() {
        let transcript = format!(
            "Title this.\n\nUser: card for Imogen Fakeworth\n\nAssistant: Happy birthday!\
             {FOOTER_SEPARATOR}answered by provider-a\nnot in the answer: Imogen Fakeworth\n\n\
             User: shorter{FOOTER_SEPARATOR}answered by local"
        );
        let mut messages = vec![
            json!({ "role": "system", "content": format!("Rules.{FOOTER_SEPARATOR}x\n\nMore rules.") }),
            json!({ "role": "user", "content": transcript }),
            json!({ "role": "user", "content": [{ "type": "text", "text": format!("a{FOOTER_SEPARATOR}b\nc") }] }),
        ];
        strip_own(&mut messages);
        assert_eq!(messages[0]["content"], "Rules.\n\nMore rules.");
        assert_eq!(
            messages[1]["content"],
            "Title this.\n\nUser: card for Imogen Fakeworth\n\nAssistant: Happy birthday!\n\nUser: shorter"
        );
        assert_eq!(messages[2]["content"][0]["text"], "a");
    }

    #[test]
    fn a_user_quoting_the_marker_is_left_alone() {
        let mut messages = vec![json!({ "role": "user", "content": "[Bullbar] said what?" })];
        strip_own(&mut messages);
        assert_eq!(messages.len(), 1);
    }
}
