use std::sync::{RwLock, RwLockWriteGuard, RwLockReadGuard, Arc};

use anymap::any::Any;
use anymap::any::CloneAny;
use anymap::Map;


pub type Dependencies = Map<CloneAny+Sync+Send>;

pub trait LockedDeps {
    fn write<T:Any+Sync+Send>(&self) -> RwLockWriteGuard<T>;
    fn read<T:Any+Sync+Send>(&self) -> RwLockReadGuard<T>;
    fn copy<T:Any+Sync+Send>(&self) -> Arc<T>;
}


impl LockedDeps for Dependencies {
    fn write<T:Any+Sync+Send>(&self) -> RwLockWriteGuard<T> {
        self.get::<Arc<RwLock<T>>>()
        .unwrap().write().unwrap()
    }
    fn read<T:Any+Sync+Send>(&self) -> RwLockReadGuard<T> {
        self.get::<Arc<RwLock<T>>>()
        .unwrap().read().unwrap()
    }
    fn copy<T:Any+Sync+Send>(&self) -> Arc<T> {
        self.get::<Arc<T>>().unwrap().clone()
    }
}
