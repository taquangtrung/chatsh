use anyhow::anyhow;
use async_stream::try_stream;
use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest::RequestBuilder;

use crate::ai::ChatEvent;

// ========================================================================
// SSE streaming helpers shared across providers
// ========================================================================

/// Return the end offset (exclusive) of the next complete SSE event in `buf`,
/// i.e. the position just past the `\n\n` separator, or `None` if no complete
/// event has arrived yet.
pub fn find_event_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2)
}

/// Send `request` and stream its `text/event-stream` body, parsing each event
/// with `parse`. `provider_id` labels transport and HTTP errors. The stream
/// emits `ChatEvent::Done` once the body ends and converts any error into a
/// terminal `ChatEvent::Error`, so providers never surface raw errors.
pub fn sse_chat_stream<F>(
    provider_id: String,
    request: RequestBuilder,
    parse: F,
) -> BoxStream<'static, ChatEvent>
where
    F: Fn(&[u8]) -> Option<ChatEvent> + Send + 'static,
{
    let stream = try_stream! {
        let resp = request
            .send()
            .await
            .map_err(|e| anyhow!("{provider_id}: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            Err(anyhow!("{provider_id}: {status}: {text}"))?;
        } else {
            let mut bytes = resp.bytes_stream();
            let mut buf = Vec::new();
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.map_err(|e| anyhow!("{provider_id}: {e}"))?;
                buf.extend_from_slice(&chunk);
                while let Some(pos) = find_event_end(&buf) {
                    let event_bytes = buf.drain(..pos).collect::<Vec<u8>>();
                    if let Some(evt) = parse(&event_bytes) {
                        yield evt;
                    }
                }
            }
            yield ChatEvent::Done;
        }
    };

    Box::pin(stream.map(|res: anyhow::Result<ChatEvent>| match res {
        Ok(ev) => ev,
        Err(e) => ChatEvent::Error {
            message: e.to_string(),
        },
    }))
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_event_end() {
        assert_eq!(find_event_end(b"data: hi\n\nrest"), Some(10));
        assert_eq!(find_event_end(b"incomplete"), None);
        assert_eq!(find_event_end(b"\n\n"), Some(2));
    }
}
