use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, RwLock,
    },
    task::{Context, Poll},
};

use futures_util::task::AtomicWaker;

struct LossyInner<T> {
    buf: RwLock<VecDeque<T>>,
    buf_size: usize,
    rx_waker: AtomicWaker,
    tx_count: AtomicUsize,
    rx_closed: AtomicBool,
}

pub(crate) struct LossySender<T> {
    inner: Arc<LossyInner<T>>,
}
impl<T> Clone for LossySender<T> {
    fn clone(&self) -> Self {
        self.inner.tx_count.fetch_add(1, Ordering::Release);
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> LossySender<T> {
    pub fn send(&self, item: T) -> Result<(), T> {
        if self.inner.rx_closed.load(Ordering::Acquire) {
            return Err(item);
        }

        let mut buf = self.inner.buf.write().unwrap();
        if buf.len() == self.inner.buf_size {
            buf.pop_front();
        }
        buf.push_back(item);
        self.inner.rx_waker.wake();
        Ok(())
    }
}
impl<T> Drop for LossySender<T> {
    fn drop(&mut self) {
        self.inner.tx_count.fetch_sub(1, Ordering::Release);
        self.inner.rx_waker.wake();
    }
}

pub(crate) struct LossyRxFut<T> {
    inner: Arc<LossyInner<T>>,
}
impl<T> Future for LossyRxFut<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.inner.tx_count.load(Ordering::Acquire) == 0 {
            Poll::Ready(None)
        } else if self.inner.buf.read().unwrap().len() > 0 {
            Poll::Ready(self.inner.buf.write().unwrap().pop_front())
        } else {
            self.inner.rx_waker.register(cx.waker());
            Poll::Pending
        }
    }
}

pub(crate) struct LossyReceiver<T> {
    inner: Arc<LossyInner<T>>,
}
impl<T> LossyReceiver<T> {
    pub fn recv(&self) -> LossyRxFut<T> {
        LossyRxFut {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Drop for LossyReceiver<T> {
    fn drop(&mut self) {
        self.inner.rx_closed.store(true, Ordering::Release);
    }
}

pub(crate) fn lossy_channel<T>(buf_size: usize) -> (LossySender<T>, LossyReceiver<T>) {
    let inner = Arc::new(LossyInner {
        tx_count: AtomicUsize::new(1),
        buf: Default::default(),
        buf_size,
        rx_waker: AtomicWaker::new(),
        rx_closed: Default::default(),
    });

    (
        LossySender {
            inner: inner.clone(),
        },
        LossyReceiver { inner },
    )
}
