use alloc::alloc::Global;
use core::{
    fmt::{self, Debug},
    hash::{BuildHasher, Hash},
};

use hashbrown::{Equivalent, TryReserveError, hash_map::DefaultHashBuilder, raw::RawTable};

pub struct HashMap<K, V> {
    hash_builder: DefaultHashBuilder,
    table: RawTable<(K, V), Global>,
}

fn make_hasher<Q, V, S>(hash_builder: &S) -> impl Fn(&(Q, V)) -> u64 + '_
where
    Q: Hash,
    S: BuildHasher,
{
    move |val| make_hash::<Q, S>(hash_builder, &val.0)
}

fn equivalent_key<Q, K, V>(k: &Q) -> impl Fn(&(K, V)) -> bool + '_
where
    Q: Equivalent<K> + ?Sized,
{
    move |x| k.equivalent(&x.0)
}

fn make_hash<Q, S>(hash_builder: &S, val: &Q) -> u64
where
    Q: Hash + ?Sized,
    S: BuildHasher,
{
    hash_builder.hash_one(val)
}

impl<K, V> HashMap<K, V> {
    pub const fn new() -> Self {
        Self {
            hash_builder: DefaultHashBuilder::new(),
            table: RawTable::new(),
        }
    }
}

#[cfg(test)]
impl<K, V> HashMap<K, V> {
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> HashMap<K, V>
where
    K: Eq + Hash,
{
    pub fn get<Q>(&self, k: &Q) -> Option<&V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.get_inner(k) {
            Some((_, v)) => Some(v),
            None => None,
        }
    }

    #[inline]
    fn get_inner<Q>(&self, k: &Q) -> Option<&(K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        if self.table.is_empty() {
            None
        } else {
            let hash = make_hash::<Q, _>(&self.hash_builder, k);
            self.table.get(hash, equivalent_key(k))
        }
    }

    pub fn contains_key<Q>(&self, k: &Q) -> bool
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.get_inner(k).is_some()
    }

    pub fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.get_inner_mut(k) {
            Some(&mut (_, ref mut v)) => Some(v),
            None => None,
        }
    }

    #[inline]
    fn get_inner_mut<Q>(&mut self, k: &Q) -> Option<&mut (K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        if self.table.is_empty() {
            None
        } else {
            let hash = make_hash::<Q, _>(&self.hash_builder, k);
            self.table.get_mut(hash, equivalent_key(k))
        }
    }

    pub fn try_insert(&mut self, k: K, v: V) -> Result<(&K, &mut V), TryReserveError> {
        let hash = make_hash::<K, _>(&self.hash_builder, &k);
        let hasher = make_hasher::<_, V, _>(&self.hash_builder);
        self.table.try_reserve(1, hasher)?;
        unsafe {
            let bucket = self.table.insert_no_grow(hash, (k, v));
            let (k_ref, v_ref) = bucket.as_mut();
            Ok((k_ref, v_ref))
        }
    }

    pub fn remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        #[allow(clippy::manual_map)]
        match self.remove_entry(k) {
            Some((_, v)) => Some(v),
            None => None,
        }
    }

    pub fn remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        let hash = make_hash::<Q, _>(&self.hash_builder, k);
        self.table.remove_entry(hash, equivalent_key(k))
    }
}

impl<K, V> Debug for HashMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashMap").finish()
    }
}
