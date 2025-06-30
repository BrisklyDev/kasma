use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone)]
/// Read-only handle
pub struct ReadHandle<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> ReadHandle<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read().unwrap()
    }
}

/// R+W handle
pub struct ReadWriteHandle<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> ReadWriteHandle<T> {
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.inner.write().unwrap()
    }

    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read().unwrap()
    }
}

pub struct SharedData<T> {
    inner: Arc<RwLock<T>>,
    pub read_write_handle: ReadWriteHandle<T>,
    pub read_handle: ReadHandle<T>,
}

/// Offers an interface for references to be read-only without having access to obtain a lock.
/// For example, a thread can hold a read-only reference but because it only has access to a
/// ReadHandle, it cannot modify the value.
impl<T> SharedData<T> {
    pub fn new(data: T) -> SharedData<T> {
        let inner = Arc::new(RwLock::new(data));
        SharedData {
            read_handle: ReadHandle {
                inner: inner.clone(),
            },
            read_write_handle: ReadWriteHandle {
                inner: inner.clone(),
            },
            inner,
        }
    }
}

/// Holds a reference and a cached (cloned) version of the vlaue. It is used to check if the cached
/// value has been outdated or not (if the reference has changed)
pub struct CachedData<'a, T: Clone + PartialEq> {
    reference: &'a T,
    cached: T,
}

impl<'a, T: Clone + PartialEq> CachedData<'a, T> {
    pub fn new(reference: &'a T) -> Self {
        CachedData {
            reference,
            cached: reference.clone(),
        }
    }

    pub fn is_cache_outdated(&self) -> bool {
        *self.reference != self.cached
    }
}
