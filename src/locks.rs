use std::collections::HashMap;

use anyhow::{Result, bail};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct ResourceLockManager {
    locks: HashMap<String, Uuid>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ResourceLock {
    pub name: String,
    pub run_id: Uuid,
}

impl ResourceLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn acquire(&mut self, name: impl Into<String>, run_id: Uuid) -> Result<ResourceLock> {
        let name = name.into();
        if let Some(owner) = self.locks.get(&name) {
            bail!("resource lock {name} already held by run {owner}");
        }

        self.locks.insert(name.clone(), run_id);
        Ok(ResourceLock { name, run_id })
    }

    pub fn release(&mut self, lock: &ResourceLock) -> Result<()> {
        match self.locks.get(&lock.name) {
            Some(owner) if *owner == lock.run_id => {
                self.locks.remove(&lock.name);
                Ok(())
            }
            Some(owner) => bail!("resource lock {} is held by run {}", lock.name, owner),
            None => bail!("resource lock {} is not held", lock.name),
        }
    }

    pub fn is_locked(&self, name: &str) -> bool {
        self.locks.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_and_releases_locks() {
        let mut manager = ResourceLockManager::new();
        let run_id = Uuid::new_v4();
        let lock = manager.acquire("database", run_id).unwrap();

        assert!(manager.acquire("database", Uuid::new_v4()).is_err());
        assert!(manager.is_locked("database"));

        manager.release(&lock).unwrap();
        assert!(!manager.is_locked("database"));
    }
}
