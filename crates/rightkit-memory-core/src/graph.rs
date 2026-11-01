//! Graph-structured in-memory knowledge graph over memory entries.
//!
//! Implements a bi-temporal knowledge graph (Zep/Graphiti-inspired, in-process).
//! Supports:
//! - Nodes with validity windows (created_at, valid_from, valid_to)
//! - Labeled edges between nodes
//! - Neighbor expansion and hybrid search
//! - Temporal slices (query valid entries at a timestamp)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A memory node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    /// Unique identifier.
    pub id: String,
    /// Node content (text).
    pub content: String,
    /// When this node was created.
    pub created_at: String,
    /// Timestamp from which this node becomes valid (inclusive).
    pub valid_from: String,
    /// Timestamp after which this node is no longer valid (exclusive).
    /// None means indefinitely valid.
    pub valid_to: Option<String>,
}

/// A directed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEdge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
    /// Edge label (e.g., "causally_related", "implements", "uses").
    ///
    /// Raw ingest may carry any label, but only [`CANONICAL_RELATIONS`] edges
    /// with resolved endpoints are traversable (see [`MemoryGraph::canonical_neighbors`]).
    /// Everything else is diagnostic and never authorizes traversal.
    pub relation: String,
    /// When this edge was created.
    pub created_at: String,
}

/// CTX-017 closed relation vocabulary. Only resolved, provenance-bound,
/// lifecycle-applicable evidence relations in this set are traversable.
/// Dangling, unresolved, or raw wikilink relations stay diagnostic and
/// non-traversable. This set must not be widened to enable CTX-039 style
/// multi-hop evidence traversal without explicit promotion.
pub const CANONICAL_RELATIONS: &[&str] = &[
    "supports",
    "contradicts",
    "supersedes",
    "derived_from",
    "enriches",
];

/// True when `relation` is a member of the closed CTX-017 vocabulary.
pub fn is_canonical_relation(relation: &str) -> bool {
    CANONICAL_RELATIONS.contains(&relation)
}

/// BM07: the durable-semantics bucket a canonical relation belongs to.
/// Distinguishing these prevents `enriches` from silently behaving like
/// `supersedes` (an enrichment must never retire a still-valid fact) and
/// prevents a `derived_from` edge from being read back as a direct
/// observation (`supports`/`contradicts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationCategory {
    /// `supersedes` — the target replaces/retires the source's validity.
    Replacement,
    /// `enriches` — adds detail to a still-live fact; MUST NOT change the
    /// target's validity window or retire it.
    Enrichment,
    /// `derived_from` — the source was computed/inferred from the target; it
    /// is not itself a direct observation and must be labeled as derived
    /// wherever it is surfaced.
    Derivation,
    /// `supports`/`contradicts` — direct observational corroboration or
    /// conflict between two nodes.
    Observation,
}

/// Classify a canonical relation into its durable-semantics bucket. Returns
/// `None` for a non-canonical relation (diagnostic-only, never traversable).
pub fn relation_category(relation: &str) -> Option<RelationCategory> {
    match relation {
        "supersedes" => Some(RelationCategory::Replacement),
        "enriches" => Some(RelationCategory::Enrichment),
        "derived_from" => Some(RelationCategory::Derivation),
        "supports" | "contradicts" => Some(RelationCategory::Observation),
        _ => None,
    }
}

/// True only for relations that assert a direct observation. A `derived_from`
/// edge is never an observation — callers must not present a derived fact as
/// if it had been directly observed (BM07 negative control).
pub fn is_observation_relation(relation: &str) -> bool {
    matches!(
        relation_category(relation),
        Some(RelationCategory::Observation)
    )
}

/// Rejection reason for a governed edge admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeRejection {
    /// Relation is outside the closed CTX-017 vocabulary.
    NonCanonicalRelation,
    /// An endpoint has no resolved node in this graph.
    DanglingEndpoint,
    /// Producer or reference missing: an evidence edge cannot be
    /// provenance-free (CTX-017).
    MissingProvenance,
}

/// Where a canonical evidence relation came from. Every durable edge carries
/// one; an edge without provenance is not admissible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationProvenance {
    /// Producer/subsystem identity that asserted the edge.
    pub producer: String,
    /// The specific evidence: event id, digest, or document reference.
    pub reference: String,
}

impl RelationProvenance {
    pub fn new(producer: impl Into<String>, reference: impl Into<String>) -> Self {
        Self {
            producer: producer.into(),
            reference: reference.into(),
        }
    }

    fn is_bound(&self) -> bool {
        !self.producer.trim().is_empty() && !self.reference.trim().is_empty()
    }
}

/// A provenance-bound canonical evidence relation held in the graph. Mirrors
/// the durable `memory_relation` row in `rightkit_memory::memdb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRelation {
    pub edge: MemoryEdge,
    pub provenance: RelationProvenance,
}

/// Why a stored evidence relation does not traverse as live evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationDiagnostic {
    /// Target has no resolved node.
    DanglingTarget,
    /// Target resolves but is not valid at the traversal timestamp
    /// (invalidated / superseded / suppressed window).
    LifecycleIneligible,
}

/// In-process bi-temporal knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGraph {
    nodes: HashMap<String, MemoryNode>,
    edges: Vec<MemoryEdge>,
    /// CTX-017 provenance-bound evidence relations. Kept separate from the
    /// permissive `edges` list so raw ingest can never masquerade as governed
    /// evidence.
    #[serde(default)]
    relations: Vec<EvidenceRelation>,
}

impl MemoryGraph {
    /// Create a new empty graph.
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            relations: Vec::new(),
        }
    }

    /// Add a node to the graph.
    pub fn add_node(&mut self, node: MemoryNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Add an edge to the graph.
    pub fn add_edge(&mut self, edge: MemoryEdge) {
        // Ensure both endpoints exist or allow dangling edges (graph is flexible).
        self.edges.push(edge);
    }

    /// Admit a governed evidence edge. Only closed-vocabulary relations with
    /// both endpoints resolved are accepted; anything else is rejected with a
    /// typed reason and must remain diagnostic, never traversable.
    pub fn add_canonical_edge(&mut self, edge: MemoryEdge) -> Result<(), EdgeRejection> {
        if !is_canonical_relation(&edge.relation) {
            return Err(EdgeRejection::NonCanonicalRelation);
        }
        if !self.nodes.contains_key(&edge.from) || !self.nodes.contains_key(&edge.to) {
            return Err(EdgeRejection::DanglingEndpoint);
        }
        self.edges.push(edge);
        Ok(())
    }

    /// Admit a provenance-bound evidence relation (CTX-017). The relation must
    /// be in the closed vocabulary, carry non-empty provenance, and have a
    /// resolved source and target. Dangling or unknown endpoints are rejected.
    ///
    /// BM07: admission NEVER mutates either endpoint's validity window,
    /// regardless of relation kind. This is the structural guarantee that an
    /// `enriches` edge cannot retire a still-valid fact — only an explicit
    /// lifecycle operation (owned by the durable store, outside this
    /// in-memory graph) can change `valid_to`.
    pub fn add_evidence_relation(
        &mut self,
        relation: EvidenceRelation,
    ) -> Result<(), EdgeRejection> {
        if !is_canonical_relation(&relation.edge.relation) {
            return Err(EdgeRejection::NonCanonicalRelation);
        }
        if !relation.provenance.is_bound() {
            return Err(EdgeRejection::MissingProvenance);
        }
        if !self.nodes.contains_key(&relation.edge.from)
            || !self.nodes.contains_key(&relation.edge.to)
        {
            return Err(EdgeRejection::DanglingEndpoint);
        }
        self.relations.retain(|existing| {
            existing.edge.from != relation.edge.from
                || existing.edge.to != relation.edge.to
                || existing.edge.relation != relation.edge.relation
        });
        self.relations.push(relation);
        Ok(())
    }

    /// Every stored evidence relation on `id` with its disposition at
    /// `timestamp`: `None` means live traversable evidence. This is the
    /// diagnostic view; dangling and lifecycle-ineligible edges appear only
    /// here.
    pub fn relation_diagnostics(
        &self,
        id: &str,
        timestamp: &str,
    ) -> Vec<(&EvidenceRelation, Option<RelationDiagnostic>)> {
        self.relations
            .iter()
            .filter(|relation| relation.edge.from == id)
            .map(|relation| {
                let diagnostic = match self.nodes.get(&relation.edge.to) {
                    None => Some(RelationDiagnostic::DanglingTarget),
                    Some(node) => {
                        let live = node.valid_from.as_str() <= timestamp
                            && node
                                .valid_to
                                .as_ref()
                                .is_none_or(|until| until.as_str() > timestamp);
                        if live {
                            None
                        } else {
                            Some(RelationDiagnostic::LifecycleIneligible)
                        }
                    }
                };
                (relation, diagnostic)
            })
            .collect()
    }

    /// Traversal view: only resolved, provenance-bound, lifecycle-applicable
    /// evidence relations valid at `timestamp`.
    pub fn evidence_neighbors(
        &self,
        id: &str,
        timestamp: &str,
    ) -> Vec<(&EvidenceRelation, &MemoryNode)> {
        self.relation_diagnostics(id, timestamp)
            .into_iter()
            .filter(|(_, diagnostic)| diagnostic.is_none())
            .filter_map(|(relation, _)| {
                self.nodes
                    .get(&relation.edge.to)
                    .map(|node| (relation, node))
            })
            .collect()
    }

    /// All stored evidence relations, regardless of disposition.
    pub fn all_evidence_relations(&self) -> &[EvidenceRelation] {
        &self.relations
    }

    /// Outgoing `derived_from` relations for `id`: facts this node was
    /// computed/inferred from. BM07: callers surfacing `id` must label it as
    /// derived (never as a direct observation) whenever this is non-empty —
    /// see [`is_observation_relation`].
    pub fn derivation_sources(&self, id: &str) -> Vec<&EvidenceRelation> {
        self.relations
            .iter()
            .filter(|relation| relation.edge.from == id && relation.edge.relation == "derived_from")
            .collect()
    }

    /// True when an edge is traversable evidence: closed-vocabulary relation
    /// with both endpoints resolved in this graph.
    pub fn is_traversable(&self, edge: &MemoryEdge) -> bool {
        is_canonical_relation(&edge.relation)
            && self.nodes.contains_key(&edge.from)
            && self.nodes.contains_key(&edge.to)
    }

    /// Get immediate neighbors of a node (1-hop expansion).
    /// Returns (edge relation, neighbor node) pairs.
    pub fn neighbors(&self, id: &str) -> Vec<(String, &MemoryNode)> {
        let mut result = Vec::new();

        for edge in &self.edges {
            if edge.from == id {
                if let Some(neighbor) = self.nodes.get(&edge.to) {
                    result.push((edge.relation.clone(), neighbor));
                }
            }
        }

        result
    }

    /// Traversal-safe neighbor view for governed evidence paths. Only
    /// closed-vocabulary edges with resolved endpoints are returned; raw,
    /// dangling, or non-canonical edges are excluded without error.
    pub fn canonical_neighbors(&self, id: &str) -> Vec<(String, &MemoryNode)> {
        self.neighbors(id)
            .into_iter()
            .filter(|(relation, neighbor)| {
                is_canonical_relation(relation)
                    && self.nodes.contains_key(id)
                    && self.nodes.contains_key(&neighbor.id)
            })
            .collect()
    }

    /// Mark a node as no longer valid from a given timestamp.
    pub fn invalidate(&mut self, id: &str, at: String) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.valid_to = Some(at);
        }
    }

    /// Get all nodes valid at a specific timestamp (temporal slice).
    /// A node is valid at time T if:
    /// - valid_from <= T
    /// - valid_to is None OR valid_to > T
    pub fn valid_at(&self, timestamp: &str) -> Vec<&MemoryNode> {
        self.nodes
            .values()
            .filter(|node| {
                node.valid_from.as_str() <= timestamp
                    && node
                        .valid_to
                        .as_ref()
                        .is_none_or(|vto| vto.as_str() > timestamp)
            })
            .collect()
    }

    /// Hybrid search: keyword search in node content + 1-hop graph expansion.
    /// Returns seed matches plus their immediate neighbors.
    pub fn hybrid_search(&self, query: &str, k: usize) -> Vec<&MemoryNode> {
        let query_lower = query.to_lowercase();
        let mut result_ids = std::collections::HashSet::new();

        // Phase 1: keyword matches (seed).
        for (id, node) in &self.nodes {
            if node.content.to_lowercase().contains(&query_lower) {
                result_ids.insert(id.clone());
            }
        }

        // Phase 2: 1-hop expansion from seeds.
        let seed_ids: Vec<String> = result_ids.iter().cloned().collect();
        for seed_id in seed_ids {
            for (_, neighbor) in self.neighbors(&seed_id) {
                result_ids.insert(neighbor.id.clone());
            }
        }

        // Collect and return up to k nodes.
        result_ids
            .into_iter()
            .take(k)
            .filter_map(|id| self.nodes.get(&id))
            .collect()
    }

    /// Get all nodes (for iteration).
    pub fn all_nodes(&self) -> Vec<&MemoryNode> {
        self.nodes.values().collect()
    }

    /// Get all edges.
    pub fn all_edges(&self) -> Vec<&MemoryEdge> {
        self.edges.iter().collect()
    }
}

impl Default for MemoryGraph {
    fn default() -> Self {
        Self::new()
    }
}
