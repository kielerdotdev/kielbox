//! Bounded shader bytecode reuse, independent of Windows and the shader compiler.
use std::sync::Arc;

const CAPACITY: usize = 16;

struct Entry<K, S> {
    key: K,
    sources: S,
    bytes: Arc<[u8]>,
}

pub(crate) struct ShaderBytecodeCache<K, S> {
    entries: Vec<Entry<K, S>>,
}

impl<K, S> Default for ShaderBytecodeCache<K, S> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K: Eq, S: Eq> ShaderBytecodeCache<K, S> {
    pub(crate) fn get_or_compile<E>(
        &mut self,
        key: K,
        sources: S,
        compile: impl FnOnce() -> Result<Arc<[u8]>, E>,
    ) -> Result<(Arc<[u8]>, bool), E> {
        let position = self.entries.iter().position(|entry| entry.key == key);
        if let Some(index) = position
            && self.entries[index].sources == sources
        {
            return Ok((self.entries[index].bytes.clone(), true));
        }

        let bytes = compile()?;
        let entry = Entry {
            key,
            sources,
            bytes: bytes.clone(),
        };
        if let Some(index) = position {
            self.entries[index] = entry;
        } else {
            if self.entries.len() == CAPACITY {
                self.entries.remove(0);
            }
            self.entries.push(entry);
        }
        Ok((bytes, false))
    }
}

#[cfg(test)]
mod tests {
    use super::{CAPACITY, ShaderBytecodeCache};
    use std::sync::Arc;

    #[test]
    fn identical_key_and_sources_reuse_the_owned_bytecode() {
        let mut cache = ShaderBytecodeCache::default();
        let (first, hit) = cache
            .get_or_compile((0, 0), [1, 2], || Ok::<_, ()>(Arc::from([1u8])))
            .unwrap();
        assert!(!hit);
        let (second, hit) = cache
            .get_or_compile((0, 0), [1, 2], || -> Result<_, ()> {
                panic!("Unchanged shader recompiled")
            })
            .unwrap();
        assert!(hit && Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn entry_stage_and_source_changes_recompile_without_accumulating_versions() {
        let mut cache = ShaderBytecodeCache::default();
        let (old, _) = cache
            .get_or_compile((0, 0), [1, 2], || Ok::<_, ()>(Arc::from([1u8])))
            .unwrap();
        for (key, sources) in [
            ((1, 0), [1, 2]),
            ((0, 1), [1, 2]),
            ((0, 0), [3, 2]),
            ((0, 0), [3, 4]),
        ] {
            assert!(
                !cache
                    .get_or_compile(key, sources, || Ok::<_, ()>(Arc::from([2u8])))
                    .unwrap()
                    .1
            );
        }
        assert_eq!(
            &*old,
            &[1],
            "Replacing cache entries must preserve outstanding bytecode owners"
        );
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn failed_recompile_never_returns_stale_bytes_and_can_be_retried() {
        let mut cache = ShaderBytecodeCache::default();
        cache
            .get_or_compile(0, [1, 2], || Ok::<_, &str>(Arc::from([1u8])))
            .unwrap();
        assert_eq!(
            cache
                .get_or_compile(0, [9, 2], || Err("Bad shader"))
                .unwrap_err(),
            "Bad shader"
        );
        let (fixed, hit) = cache
            .get_or_compile(0, [9, 2], || Ok::<_, &str>(Arc::from([9u8])))
            .unwrap();
        assert!(!hit);
        assert_eq!(&*fixed, &[9]);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn distinct_keys_evict_the_oldest_entry_at_capacity() {
        let mut cache = ShaderBytecodeCache::default();
        for key in 0..CAPACITY + 4 {
            cache
                .get_or_compile(key, (), || Ok::<_, ()>(Arc::from([key as u8])))
                .unwrap();
            assert!(cache.entries.len() <= CAPACITY);
        }
        assert_eq!(cache.entries.first().unwrap().key, 4);
        assert!(
            !cache
                .get_or_compile(0, (), || Ok::<_, ()>(Arc::from([0u8])))
                .unwrap()
                .1
        );
        assert_eq!(cache.entries.len(), CAPACITY);
    }
}
