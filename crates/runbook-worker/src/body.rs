//! CF12 review round 1: request bodies read with a running cap. A declared
//! `Content-Length` over the cap is refused before any byte is read (F45);
//! a body without one (chunked) is read chunk by chunk and refused as soon
//! as it passes the cap, so the Worker never holds more than the cap plus
//! one chunk, whatever the client sends.
use futures_util::{Stream, StreamExt};

/// What a capped read found.
#[derive(Debug, PartialEq, Eq)]
pub enum Capped {
    /// The whole body, at most the cap.
    Body(Vec<u8>),
    /// Over the cap, declared or as read; the rest was never read.
    TooLarge,
}

/// Read `chunks` until they end (the body) or pass `cap` bytes (refused,
/// unread past that chunk). A failed read is an empty body, as before.
pub async fn collect<S, E>(chunks: S, cap: u64) -> Capped
where
    S: Stream<Item = Result<Vec<u8>, E>>,
{
    collect_watched(chunks, cap, &|| {}).await
}

/// [`collect`], calling `progress` as each chunk arrives (CF12b review
/// round 1: an import's hold on the isolate stays fresh while a slow
/// client sends its archive).
pub async fn collect_watched<S, E>(chunks: S, cap: u64, progress: &dyn Fn()) -> Capped
where
    S: Stream<Item = Result<Vec<u8>, E>>,
{
    let mut chunks = std::pin::pin!(chunks);
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = chunks.next().await {
        progress();
        let Ok(chunk) = chunk else {
            return Capped::Body(Vec::new());
        };
        if (body.len() + chunk.len()) as u64 > cap {
            return Capped::TooLarge;
        }
        body.extend_from_slice(&chunk);
    }
    Capped::Body(body)
}

/// The declared length's verdict, before any byte is read: over the cap,
/// or to be read.
pub fn declared_over(content_length: Option<&str>, cap: u64) -> bool {
    content_length
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|n| n > cap)
}

/// The request's body, at most `cap` bytes: refused unread when its
/// declared length is over, else read with a running cap.
#[cfg(target_arch = "wasm32")]
pub async fn read(req: &mut worker::Request, cap: u64) -> worker::Result<Capped> {
    read_watched(req, cap, &|| {}).await
}

/// [`read`], calling `progress` as each chunk arrives.
#[cfg(target_arch = "wasm32")]
pub async fn read_watched(
    req: &mut worker::Request,
    cap: u64,
    progress: &dyn Fn(),
) -> worker::Result<Capped> {
    if declared_over(req.headers().get("content-length")?.as_deref(), cap) {
        return Ok(Capped::TooLarge);
    }
    // No body (a GET, or an empty POST): nothing to read.
    let Ok(stream) = req.stream() else {
        return Ok(Capped::Body(Vec::new()));
    };
    Ok(collect_watched(stream, cap, progress).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use std::cell::Cell;

    fn ready<T>(f: impl std::future::Future<Output = T>) -> T {
        let mut f = std::pin::pin!(f);
        match f
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("synchronous"),
        }
    }

    #[test]
    fn a_body_at_the_cap_is_read_whole() {
        let chunks = stream::iter(vec![Ok::<_, ()>(vec![1; 6]), Ok(vec![2; 4])]);
        assert_eq!(
            ready(collect(chunks, 10)),
            Capped::Body([vec![1; 6], vec![2; 4]].concat())
        );
    }

    #[test]
    fn reading_stops_at_the_chunk_past_the_cap() {
        let pulled = Cell::new(0);
        let chunks = stream::iter(0..1_000).map(|_| {
            pulled.set(pulled.get() + 1);
            Ok::<_, ()>(vec![0; 4])
        });
        assert_eq!(ready(collect(chunks, 10)), Capped::TooLarge);
        assert_eq!(
            pulled.get(),
            3,
            "10 bytes fit in two chunks; the third passes"
        );
    }

    /// CF12b review round 1: each chunk that arrives is progress.
    #[test]
    fn each_chunk_read_is_progress() {
        let seen = Cell::new(0);
        let chunks = stream::iter(vec![Ok::<_, ()>(vec![1; 3]); 4]);
        let body = ready(collect_watched(chunks, 100, &|| seen.set(seen.get() + 1)));
        assert_eq!(body, Capped::Body(vec![1; 12]));
        assert_eq!(seen.get(), 4);
    }

    #[test]
    fn a_declared_length_over_the_cap_is_refused_before_reading() {
        assert!(declared_over(Some("11"), 10));
        assert!(!declared_over(Some("10"), 10));
        assert!(!declared_over(None, 10));
        assert!(!declared_over(Some("chunked"), 10));
    }
}
