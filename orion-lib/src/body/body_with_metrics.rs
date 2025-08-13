// SPDX-FileCopyrightText: © 2025 Huawei Cloud Computing Technologies Co., Ltd
// SPDX-License-Identifier: Apache-2.0
//
// Copyright 2025 Huawei Cloud Computing Technologies Co., Ltd
//
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
//

use bytes::Buf;
use http_body::{Body, Frame, SizeHint};
use parking_lot::Mutex;
use pin_project::pin_project;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use crate::body::response_flags::{BodyKind, ResponseFlags};

type MetricsClosure = Box<dyn FnOnce(u64, ResponseFlags) + Send + 'static>;

pub struct MetricsState {
    kind: BodyKind,
    bytes_counter: AtomicU64,
    on_complete: Mutex<Option<MetricsClosure>>,
}

/// Pin-project prevents the struct to implement `Drop`.
/// This workaround allows us to use `Drop` and invoke the closure, if not already executed.
#[derive(Clone)]
pub struct DropGuard {
    state: Arc<MetricsState>,
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        trigger_on_complete(&self.state, ResponseFlags::default());
    }
}

fn trigger_on_complete(state: &Arc<MetricsState>, flags: ResponseFlags) {
    let mut guard = state.on_complete.lock();
    if let Some(closure) = guard.take() {
        let bytes = state.bytes_counter.load(Ordering::Relaxed);
        closure(bytes, flags);
    }
}

#[pin_project]
pub struct BodyWithMetrics<B> {
    #[pin]
    pub inner: B,
    pub state: Arc<MetricsState>,
    pub guard: DropGuard,
}

impl<B> BodyWithMetrics<B> {
    pub fn new<F>(kind: BodyKind, inner: B, on_complete: F) -> Self
    where
        F: FnOnce(u64, ResponseFlags) + Send + 'static,
    {
        let state = Arc::new(MetricsState {
            kind,
            bytes_counter: AtomicU64::new(0),
            on_complete: Mutex::new(Some(Box::new(on_complete))),
        });

        Self { inner, guard: DropGuard { state: state.clone() }, state }
    }

    pub fn map_into<B2>(self) -> BodyWithMetrics<B2>
    where
        B: Into<B2>,
    {
        BodyWithMetrics { inner: self.inner.into(), state: self.state, guard: self.guard }
    }
}

impl<B> Body for BodyWithMetrics<B>
where
    B: Body,
    ResponseFlags: for<'a> From<(&'a <B as Body>::Error, BodyKind)>,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let poll = this.inner.poll_frame(cx);
        match &poll {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let size = data.remaining() as u64;
                    this.state.bytes_counter.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
                }
            },
            Poll::Ready(None) => {
                trigger_on_complete(this.state, ResponseFlags::default());
            },
            Poll::Ready(Some(Err(err))) => {
                let flags = ResponseFlags::from((err, this.state.kind));
                trigger_on_complete(this.state, flags);
            },
            Poll::Pending => {},
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
