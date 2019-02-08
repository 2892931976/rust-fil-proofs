use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem;
use std::sync::{Arc, RwLock};

use crate::crypto::feistel::{self, FeistelPrecomputed};
use crate::drgraph::{BucketGraph, Graph};
use crate::hasher::Hasher;
use crate::layered_drgporep::Layerable;
use crate::parameter_cache::ParameterSetIdentifier;
use crate::SP_LOG;

pub const DEFAULT_EXPANSION_DEGREE: usize = 8;

// Maximum size of cache used for parents, expressed in MiB. This
// value is just an approximation, it's not actually measured.
pub const MAX_CACHE_SIZE: usize = 10;
// TODO: Arbitrarily chosen for tests.

// Cache of node's parents.
pub type ParentCache = HashMap<usize, Vec<usize>>;
// TODO: Using `usize` as its the dominant type throughout the
// code, but it should be reconciled with the underlying `u32`
// used in Feistel.

// ZigZagGraph will hold two different (but related) `ParentCache`,
// the first one for the `forward` direction and the second one
// for the `reversed`.
pub type TwoWayParentCache = Vec<ParentCache>;

// The cache is hold in an `Arc` to make it available across different
// threads. It is accessed through a `RwLock` to distinguish between
// read an write operations.
pub type ShareableParentCache = Arc<RwLock<TwoWayParentCache>>;
// TODO: Evaluate decoupling the two caches in different `RwLock` to reduce
// contention. At the moment they are joined under the same lock for simplicity
// since `transform_and_replicate_layers` even in the parallel case seems to
// generate the parents (`vde::encode`) of the different layers sequentially.

#[derive(Debug, Clone)]
pub struct ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H> + 'static,
{
    expansion_degree: usize,
    base_graph: G,
    pub reversed: bool,
    feistel_precomputed: FeistelPrecomputed,

    // This parents cache is currently used for the *expanded parents only*, generated
    // by the expensive Feistel operations in the ZigZag, it doesn't contain the
    // "base" (in the `Graph` terminology) parents, which are cheaper to compute.
    // This is not an LRU cache, it holds the first `cache_entries` of the total
    // possible `base_graph.size()` (the assumption here is that we either request
    // all entries sequentially when encoding or any random entry once when proving
    // or verifying, but there's no locality to take advantage of so keep the logic
    // as simple as possible).
    parents_cache: ShareableParentCache,
    // Keep the size of the cache outside the lock to be easily accessible.
    cache_entries: usize,
    // TODO: Evaluate integrating `cache_entries` into a separate cache structure.
    _h: PhantomData<H>,
}

pub type ZigZagBucketGraph<H> = ZigZagGraph<H, BucketGraph<H>>;

impl<'a, H, G> Layerable<H> for ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H> + 'static,
{
}

impl<H, G> ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H>,
{
    pub fn new(
        base_graph: Option<G>,
        nodes: usize,
        base_degree: usize,
        expansion_degree: usize,
        seed: [u32; 7],
    ) -> Self {
        // Estimate the maximum number of entries the parents cache can
        // have.
        let size_bytes = MAX_CACHE_SIZE << 20;
        // For now, we always use `usize` to index nodes
        // independently of the actual maximum size in `nodes`.
        let index_size = mem::size_of::<usize>();
        // TODO: Optimization: Use smaller indexes when possible.
        // We actually have two caches, one in the forward and one in the
        // reversed direction.
        let size_per_direction = size_bytes / 2;
        // For each entry in the cache there will be (approximately)
        // `expansion_degree` parents per node (actually some nodes may
        // have less than that).
        let cache_max_entries = size_per_direction / (expansion_degree * index_size);
        // TODO: This is underestimating the size of the `HashMap` in `ParentCache`
        // and focusing on the size of the underlying buffer in the `Vec`. The
        // `ParentCache` needs to be converted to a more compact structure, like
        // a `[u32]` (or `[u8]` if we use smaller indexes when possible).

        let cache_entries = if nodes > cache_max_entries {
            info!(
                SP_LOG,
                "using a cache smaller ({}) than the number of nodes ({})",
                cache_max_entries,
                nodes
            );
            cache_max_entries
        } else {
            nodes
        };

        ZigZagGraph {
            base_graph: match base_graph {
                Some(graph) => graph,
                None => G::new(nodes, base_degree, 0, seed),
            },
            expansion_degree,
            reversed: false,
            feistel_precomputed: feistel::precompute((expansion_degree * nodes) as u32),
            parents_cache: Arc::new(RwLock::new(vec![
                HashMap::with_capacity(cache_entries),
                HashMap::with_capacity(cache_entries),
            ])),
            cache_entries,
            _h: PhantomData,
        }
    }
}

impl<H, G> ParameterSetIdentifier for ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H> + ParameterSetIdentifier,
{
    fn parameter_set_identifier(&self) -> String {
        format!(
            "zigzag_graph::ZigZagGraph{{expansion_degree: {} base_graph: {} }}",
            self.expansion_degree,
            self.base_graph.parameter_set_identifier()
        )
    }
}

pub trait ZigZag: ::std::fmt::Debug + Clone + PartialEq + Eq {
    type BaseHasher: Hasher;
    type BaseGraph: Graph<Self::BaseHasher>;

    /// zigzag returns a new graph with expansion component inverted and a distinct
    /// base DRG graph -- with the direction of drg connections reversed. (i.e. from high-to-low nodes).
    /// The name is 'weird', but so is the operation -- hence the choice.
    fn zigzag(&self) -> Self;
    /// Constructs a new graph.
    fn base_graph(&self) -> Self::BaseGraph;
    fn expansion_degree(&self) -> usize;
    fn reversed(&self) -> bool;
    fn expanded_parents(&self, node: usize) -> Vec<usize>;
    fn real_index(&self, i: usize) -> usize;
    fn new_zigzag(
        nodes: usize,
        base_degree: usize,
        expansion_degree: usize,
        seed: [u32; 7],
    ) -> Self;
}

impl<Z: ZigZag> Graph<Z::BaseHasher> for Z {
    fn size(&self) -> usize {
        self.base_graph().size()
    }

    fn degree(&self) -> usize {
        self.base_graph().degree() + self.expansion_degree()
    }

    #[inline]
    fn parents(&self, raw_node: usize) -> Vec<usize> {
        // If graph is reversed, use real_index to convert index to reversed index.
        // So we convert a raw reversed node to an unreversed node, calculate its parents,
        // then convert the parents to reversed.

        let drg_parents = self
            .base_graph()
            .parents(self.real_index(raw_node))
            .iter()
            .map(|i| self.real_index(*i))
            .collect::<Vec<_>>();

        let mut parents = drg_parents;
        // expanded_parents takes raw_node
        let expanded_parents = self.expanded_parents(raw_node);

        parents.extend(expanded_parents.iter());

        // Pad so all nodes have correct degree.
        for _ in 0..(self.degree() - parents.len()) {
            if self.reversed() {
                parents.push(self.size() - 1);
            } else {
                parents.push(0);
            }
        }
        assert!(parents.len() == self.degree());
        parents.sort();

        assert!(parents.iter().all(|p| if self.forward() {
            *p <= raw_node
        } else {
            *p >= raw_node
        }));

        parents
    }

    fn seed(&self) -> [u32; 7] {
        self.base_graph().seed()
    }

    fn new(nodes: usize, base_degree: usize, expansion_degree: usize, seed: [u32; 7]) -> Self {
        Z::new_zigzag(nodes, base_degree, expansion_degree, seed)
    }

    fn forward(&self) -> bool {
        !self.reversed()
    }
}

impl<'a, H, G> ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H>,
{
    fn correspondent(&self, node: usize, i: usize) -> usize {
        let a = (node * self.expansion_degree) as u32 + i as u32;
        let feistel_keys = &[1, 2, 3, 4];

        let transformed = if self.reversed {
            feistel::invert_permute(
                self.size() as u32 * self.expansion_degree as u32,
                a,
                feistel_keys,
                self.feistel_precomputed,
            )
        } else {
            feistel::permute(
                self.size() as u32 * self.expansion_degree as u32,
                a,
                feistel_keys,
                self.feistel_precomputed,
            )
        };
        transformed as usize / self.expansion_degree
    }

    // The first cache in `parents_cache` corresponds to the forward direction,
    // the second one to the reversed.
    fn get_cache_index(&self) -> usize {
        if self.forward() {
            0
        } else {
            1
        }
    }

    // Read the `node` entry in the parents cache (which may not exist) for
    // the current direction set in the graph and return a copy of it (or
    // `None` to signal a cache miss).
    fn read_parents_cache(&self, node: usize) -> Option<Vec<usize>> {
        // If the index exceeds the cache size don't bother checking.
        if node > self.cache_entries {
            return None;
        }

        let read_lock = self.parents_cache.read().unwrap();

        let parents_cache = &(*read_lock)[self.get_cache_index()];

        if let Some(parents) = parents_cache.get(&node) {
            Some(parents.clone())
        } else {
            None
        }
    }

    // Save the `parents` of the `node` in its entry of the cache.
    fn write_parents_cache(&self, node: usize, parents: Vec<usize>) {
        // Don't allow writing more entries than the already allocated space.
        if node > self.cache_entries {
            return;
        }

        let mut write_lock = self.parents_cache.write().unwrap();

        let parents_cache = &mut (*write_lock)[self.get_cache_index()];

        let old_value = parents_cache.insert(node, parents);

        debug_assert_eq!(old_value, None);
        // We shouldn't be rewriting entries (with most likely the same values),
        // this would be a clear indication of a bug.
    }
}

impl<'a, H, G> ZigZag for ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H>,
{
    type BaseHasher = H;
    type BaseGraph = G;

    fn new_zigzag(
        nodes: usize,
        base_degree: usize,
        expansion_degree: usize,
        seed: [u32; 7],
    ) -> Self {
        Self::new(None, nodes, base_degree, expansion_degree, seed)
    }

    /// To zigzag a graph, we just toggle its reversed field.
    /// All the real work happens when we calculate node parents on-demand.
    // We always share the two caches (forward/reversed) between
    // ZigZag graphs even if each graph will use only one of those
    // caches (depending of its direction). This allows to propagate
    // the caches across different layers, where consecutive even+odd
    // layers have inverse directions.
    fn zigzag(&self) -> Self {
        let mut zigzag = self.clone();
        zigzag.reversed = !zigzag.reversed;
        zigzag
    }

    fn base_graph(&self) -> Self::BaseGraph {
        self.base_graph.clone()
    }

    fn expansion_degree(&self) -> usize {
        self.expansion_degree
    }

    fn reversed(&self) -> bool {
        self.reversed
    }

    // TODO: Optimization: Evaluate providing an `all_parents` (and hence
    // `all_expanded_parents`) method that would return the entire cache
    // in a single lock operation, or at least (if the cache is not big enough)
    // it would allow to batch parents calculations with that single lock. Also,
    // since there is a reciprocity between forward and reversed parents,
    // we would only need to compute the parents in one direction and with
    // that fill both caches.
    #[inline]
    fn expanded_parents(&self, node: usize) -> Vec<usize> {
        if let Some(parents) = self.read_parents_cache(node) {
            return parents;
        }

        let parents: Vec<usize> = (0..self.expansion_degree)
            .filter_map(|i| {
                let other = self.correspondent(node, i);
                if self.reversed {
                    if other > node {
                        Some(other)
                    } else {
                        None
                    }
                } else if other < node {
                    Some(other)
                } else {
                    None
                }
            })
            .collect();

        self.write_parents_cache(node, parents.clone());

        parents
    }

    #[inline]
    fn real_index(&self, i: usize) -> usize {
        if self.reversed {
            (self.size() - 1) - i
        } else {
            i
        }
    }
}

impl<H, G> PartialEq for ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H>,
{
    fn eq(&self, other: &ZigZagGraph<H, G>) -> bool {
        self.base_graph == other.base_graph
            && self.expansion_degree == other.expansion_degree
            && self.reversed == other.reversed
    }
}

impl<H, G> Eq for ZigZagGraph<H, G>
where
    H: Hasher,
    G: Graph<H>,
{
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use crate::drgraph::new_seed;
    use crate::hasher::{Blake2sHasher, PedersenHasher, Sha256Hasher};

    fn assert_graph_ascending<H: Hasher, G: Graph<H>>(g: G) {
        for i in 0..g.size() {
            for p in g.parents(i) {
                if i == 0 {
                    assert!(p == i);
                } else {
                    assert!(p < i);
                }
            }
        }
    }

    fn assert_graph_descending<H: Hasher, G: Graph<H>>(g: G) {
        for i in 0..g.size() {
            let parents = g.parents(i);
            for p in parents {
                if i == g.size() - 1 {
                    assert!(p == i);
                } else {
                    assert!(p > i);
                }
            }
        }
    }

    #[test]
    fn zigzag_graph_zigzags_pedersen() {
        test_zigzag_graph_zigzags::<PedersenHasher>();
    }

    #[test]
    fn zigzag_graph_zigzags_sha256() {
        test_zigzag_graph_zigzags::<Sha256Hasher>();
    }

    #[test]
    fn zigzag_graph_zigzags_blake2s() {
        test_zigzag_graph_zigzags::<Blake2sHasher>();
    }

    fn test_zigzag_graph_zigzags<H: 'static + Hasher>() {
        let g = ZigZagBucketGraph::<H>::new_zigzag(50, 5, DEFAULT_EXPANSION_DEGREE, new_seed());
        let gz = g.zigzag();

        assert_graph_ascending(g);
        assert_graph_descending(gz);
    }

    #[test]
    fn expansion_pedersen() {
        test_expansion::<PedersenHasher>();
    }

    #[test]
    fn expansion_sha256() {
        test_expansion::<Sha256Hasher>();
    }

    #[test]
    fn expansion_blake2s() {
        test_expansion::<Blake2sHasher>();
    }

    fn test_expansion<H: 'static + Hasher>() {
        // We need a graph.
        let g = ZigZagBucketGraph::<H>::new_zigzag(25, 5, DEFAULT_EXPANSION_DEGREE, new_seed());

        // We're going to fully realize the expansion-graph component, in a HashMap.
        let gcache = get_all_expanded_parents(&g);

        // Here's the zigzag version of the graph.
        let gz = g.zigzag();

        // And a HashMap to hold the expanded parents.
        let gzcache = get_all_expanded_parents(&gz);

        for i in 0..gz.size() {
            let parents = gzcache.get(&i).unwrap();

            // Check to make sure all (expanded) node-parent relationships also exist in reverse,
            // in the original graph's Hashmap.
            for p in parents {
                assert!(gcache[&p].contains(&i));
            }
        }

        // And then do the same check to make sure all (expanded) node-parent relationships from the original
        // are present in the zigzag, just reversed.
        for i in 0..g.size() {
            let parents = g.expanded_parents(i);
            for p in parents {
                assert!(gzcache[&p].contains(&i));
            }
        }
        // Having checked both ways, we know the graph and its zigzag counterpart have 'expanded' components
        // which are each other's inverses. It's important that this be true.
    }

    fn get_all_expanded_parents<H: 'static + Hasher>(
        zigzag_graph: &ZigZagBucketGraph<H>,
    ) -> HashMap<usize, Vec<usize>> {
        let mut parents_map: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..zigzag_graph.size() {
            parents_map.insert(i, zigzag_graph.expanded_parents(i));
        }

        assert_eq!(get_cache_size(&zigzag_graph), zigzag_graph.size());
        // This will only work if the `MAX_CACHE_SIZE` if big enough for the examples
        // (which should normally be so).

        parents_map
    }

    fn get_cache_size<H: 'static + Hasher>(zigzag_graph: &ZigZagBucketGraph<H>) -> usize {
        let parents_cache_lock = zigzag_graph.parents_cache.read().unwrap();
        (*parents_cache_lock)[zigzag_graph.get_cache_index()].len()
    }
}
