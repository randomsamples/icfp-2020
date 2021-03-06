use core::fmt;
use core::pin::Pin;
use futures_core::future::Future;
use futures_core::stream::{Stream, FusedStream};
use futures_core::task::{Context, Poll};
#[cfg(feature = "sink")]
use futures_sink::Sink;
use pin_project::{pin_project, project};

/// Stream for the [`take_while`](super::StreamExt::take_while) method.
#[pin_project]
#[must_use = "streams do nothing unless polled"]
pub struct TakeWhile<St: Stream, Fut, F> {
    #[pin]
    stream: St,
    f: F,
    #[pin]
    pending_fut: Option<Fut>,
    pending_item: Option<St::Item>,
    done_taking: bool,
}

impl<St, Fut, F> fmt::Debug for TakeWhile<St, Fut, F>
where
    St: Stream + fmt::Debug,
    St::Item: fmt::Debug,
    Fut: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TakeWhile")
            .field("stream", &self.stream)
            .field("pending_fut", &self.pending_fut)
            .field("pending_item", &self.pending_item)
            .field("done_taking", &self.done_taking)
            .finish()
    }
}

impl<St, Fut, F> TakeWhile<St, Fut, F>
    where St: Stream,
          F: FnMut(&St::Item) -> Fut,
          Fut: Future<Output = bool>,
{
    pub(super) fn new(stream: St, f: F) -> TakeWhile<St, Fut, F> {
        TakeWhile {
            stream,
            f,
            pending_fut: None,
            pending_item: None,
            done_taking: false,
        }
    }

    delegate_access_inner!(stream, St, ());
}

impl<St, Fut, F> Stream for TakeWhile<St, Fut, F>
    where St: Stream,
          F: FnMut(&St::Item) -> Fut,
          Fut: Future<Output = bool>,
{
    type Item = St::Item;

    #[project]
    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<St::Item>> {
        if self.done_taking {
            return Poll::Ready(None);
        }

        #[project]
        let TakeWhile { mut stream, f, mut pending_fut, pending_item, done_taking } = self.project();

        Poll::Ready(loop {
            if let Some(fut) = pending_fut.as_mut().as_pin_mut() {
                let take = ready!(fut.poll(cx));
                let item = pending_item.take();
                pending_fut.set(None);
                if take {
                    break item;
                } else {
                    *done_taking = true;
                    break None;
                }
            } else if let Some(item) = ready!(stream.as_mut().poll_next(cx)) {
                pending_fut.set(Some(f(&item)));
                *pending_item = Some(item);
            } else {
                break None;
            }
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.done_taking {
            return (0, Some(0));
        }

        let pending_len = if self.pending_item.is_some() { 1 } else { 0 };
        let (_, upper) = self.stream.size_hint();
        let upper = match upper {
            Some(x) => x.checked_add(pending_len),
            None => None,
        };
        (0, upper) // can't know a lower bound, due to the predicate
    }
}

impl<St, Fut, F> FusedStream for TakeWhile<St, Fut, F>
    where St: FusedStream,
          F: FnMut(&St::Item) -> Fut,
          Fut: Future<Output = bool>,
{
    fn is_terminated(&self) -> bool {
        self.done_taking || self.pending_item.is_none() && self.stream.is_terminated()
    }
}

// Forwarding impl of Sink from the underlying stream
#[cfg(feature = "sink")]
impl<S, Fut, F, Item> Sink<Item> for TakeWhile<S, Fut, F>
    where S: Stream + Sink<Item>,
{
    type Error = S::Error;

    delegate_sink!(stream, Item);
}
