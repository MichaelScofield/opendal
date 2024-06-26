// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use crate::raw::*;
use crate::*;
use bytes::Buf;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

/// BufferSink is the adapter of [`Sink`] generated by [`Writer`].
///
/// Users can use this adapter in cases where they need to use [`Sink`]
pub struct BufferSink {
    state: State,
    buf: Buffer,
}

enum State {
    Idle(Option<oio::Writer>),
    Writing(BoxedStaticFuture<(oio::Writer, Result<usize>)>),
    Closing(BoxedStaticFuture<(oio::Writer, Result<()>)>),
}

/// # Safety
///
/// FuturesReader only exposes `&mut self` to the outside world, so it's safe to be `Sync`.
unsafe impl Sync for State {}

impl BufferSink {
    /// Create a new sink from a [`oio::Writer`].
    #[inline]
    pub(crate) fn new(w: oio::Writer) -> Self {
        BufferSink {
            state: State::Idle(Some(w)),
            buf: Buffer::new(),
        }
    }
}

impl futures::Sink<Buffer> for BufferSink {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_flush(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Buffer) -> Result<()> {
        self.buf = item;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                State::Idle(w) => {
                    if this.buf.is_empty() {
                        return Poll::Ready(Ok(()));
                    }
                    let Some(mut w) = w.take() else {
                        return Poll::Ready(Err(Error::new(
                            ErrorKind::Unexpected,
                            "state invalid: sink has been closed",
                        )));
                    };
                    let buf = this.buf.clone();
                    let fut = async move {
                        let res = w.write_dyn(buf).await;
                        (w, res)
                    };
                    this.state = State::Writing(Box::pin(fut));
                }
                State::Writing(fut) => {
                    let (w, res) = ready!(fut.as_mut().poll(cx));
                    this.state = State::Idle(Some(w));
                    match res {
                        Ok(n) => {
                            this.buf.advance(n);
                        }
                        Err(err) => return Poll::Ready(Err(err)),
                    }
                }
                State::Closing(_) => {
                    return Poll::Ready(Err(Error::new(
                        ErrorKind::Unexpected,
                        "state invalid: sink is closing",
                    )))
                }
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                State::Idle(w) => {
                    let Some(mut w) = w.take() else {
                        return Poll::Ready(Err(Error::new(
                            ErrorKind::Unexpected,
                            "state invalid: sink has been closed",
                        )));
                    };

                    if this.buf.is_empty() {
                        let fut = async move {
                            let res = w.close_dyn().await;
                            (w, res)
                        };
                        this.state = State::Closing(Box::pin(fut));
                    } else {
                        let buf = this.buf.clone();
                        let fut = async move {
                            let res = w.write_dyn(buf).await;
                            (w, res)
                        };
                        this.state = State::Writing(Box::pin(fut));
                    }
                }
                State::Writing(fut) => {
                    let (w, res) = ready!(fut.as_mut().poll(cx));
                    this.state = State::Idle(Some(w));
                    match res {
                        Ok(n) => {
                            this.buf.advance(n);
                        }
                        Err(err) => return Poll::Ready(Err(err)),
                    }
                }
                State::Closing(fut) => {
                    let (w, res) = ready!(fut.as_mut().poll(cx));
                    this.state = State::Idle(Some(w));
                    match res {
                        Ok(_) => {
                            this.state = State::Idle(None);
                            return Poll::Ready(Ok(()));
                        }
                        Err(err) => return Poll::Ready(Err(err)),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::raw::MaybeSend;
    use crate::BufferSink;

    #[test]
    fn test_trait() {
        let v = BufferSink::new(Box::new(()));

        let _: Box<dyn Unpin + MaybeSend + Sync + 'static> = Box::new(v);
    }
}
