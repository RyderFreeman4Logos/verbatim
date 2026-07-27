//! Structured research decomposition: questions, subquestions, plans.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::error::{ResearchError, ResearchResult};
use super::util::{require_digest, require_non_empty};

/// Opaque subquestion identifier within a decomposition plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubQuestionId(pub String);

impl SubQuestionId {
    pub fn new(raw: impl Into<String>) -> ResearchResult<Self> {
        let raw = raw.into();
        require_non_empty("subquestion_id", &raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("subquestion_id", &self.0)
    }
}

/// Top-level research question submitted to multi-hop research.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchQuestion {
    pub question_id: String,
    pub text: String,
    /// Required facts the research must cover (opaque labels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_facts: Vec<String>,
    /// Required relations the research must cover (opaque labels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_relations: Vec<String>,
    /// Optional QueryPlan content hash when bound to a wire QueryPlan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_plan_hash: Option<String>,
}

/// Field bundle for [`ResearchQuestion::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchQuestionFields {
    pub question_id: String,
    pub text: String,
    pub required_facts: Vec<String>,
    pub required_relations: Vec<String>,
    pub query_plan_hash: Option<String>,
}

impl ResearchQuestion {
    pub fn new(fields: ResearchQuestionFields) -> ResearchResult<Self> {
        let q = Self {
            question_id: fields.question_id,
            text: fields.text,
            required_facts: fields.required_facts,
            required_relations: fields.required_relations,
            query_plan_hash: fields.query_plan_hash,
        };
        q.validate()?;
        Ok(q)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("question_id", &self.question_id)?;
        require_non_empty("research_question.text", &self.text)?;
        for fact in &self.required_facts {
            require_non_empty("required_fact", fact)?;
        }
        for rel in &self.required_relations {
            require_non_empty("required_relation", rel)?;
        }
        if let Some(h) = &self.query_plan_hash {
            require_digest("query_plan_hash", h)?;
        }
        Ok(())
    }
}

/// Retriever class allowed for a subquery (parallel multi-source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverKind {
    Lexical,
    Dense,
    GraphLocal,
    GraphGlobal,
    Exact,
}

impl RetrieverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Dense => "dense",
            Self::GraphLocal => "graph_local",
            Self::GraphGlobal => "graph_global",
            Self::Exact => "exact",
        }
    }

    /// Graph retrievers require every edge to have backing evidence (residual
    /// enforcement in adapters; contract flags the class).
    pub fn requires_edge_evidence(self) -> bool {
        matches!(self, Self::GraphLocal | Self::GraphGlobal)
    }
}

/// One typed subquestion with declared dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubQuestion {
    pub id: SubQuestionId,
    pub text: String,
    /// Subquestions that must complete before this one may retrieve (DAG edges).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<SubQuestionId>,
    /// Preferred retrievers for this subquestion (non-empty).
    pub retrievers: Vec<RetrieverKind>,
    /// Required fact labels this subquestion is intended to cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets_facts: Vec<String>,
    /// Required relation labels this subquestion is intended to cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets_relations: Vec<String>,
}

/// Field bundle for [`SubQuestion::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubQuestionFields {
    pub id: String,
    pub text: String,
    pub depends_on: Vec<String>,
    pub retrievers: Vec<RetrieverKind>,
    pub targets_facts: Vec<String>,
    pub targets_relations: Vec<String>,
}

impl SubQuestion {
    pub fn new(fields: SubQuestionFields) -> ResearchResult<Self> {
        let mut depends_on = Vec::with_capacity(fields.depends_on.len());
        for d in fields.depends_on {
            depends_on.push(SubQuestionId::new(d)?);
        }
        let sq = Self {
            id: SubQuestionId::new(fields.id)?,
            text: fields.text,
            depends_on,
            retrievers: fields.retrievers,
            targets_facts: fields.targets_facts,
            targets_relations: fields.targets_relations,
        };
        sq.validate()?;
        Ok(sq)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        self.id.validate()?;
        require_non_empty("subquestion.text", &self.text)?;
        if self.retrievers.is_empty() {
            return Err(ResearchError::validation(
                "subquestion requires at least one retriever",
            ));
        }
        for dep in &self.depends_on {
            dep.validate()?;
            if dep == &self.id {
                return Err(ResearchError::validation(
                    "subquestion must not depend on itself",
                ));
            }
        }
        // Duplicate depends_on is invalid.
        let mut seen = BTreeSet::new();
        for dep in &self.depends_on {
            if !seen.insert(dep.clone()) {
                return Err(ResearchError::validation(format!(
                    "duplicate depends_on entry {}",
                    dep.as_str()
                )));
            }
        }
        for fact in &self.targets_facts {
            require_non_empty("targets_fact", fact)?;
        }
        for rel in &self.targets_relations {
            require_non_empty("targets_relation", rel)?;
        }
        Ok(())
    }
}

/// Structured decomposition plan produced from a [`ResearchQuestion`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecompositionPlan {
    pub plan_id: String,
    pub research_question_id: String,
    /// Content hash of the ResearchQuestion (or bound QueryPlan when present).
    pub research_question_hash: String,
    pub subquestions: Vec<SubQuestion>,
}

/// Field bundle for [`DecompositionPlan::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompositionPlanFields {
    pub plan_id: String,
    pub research_question_id: String,
    pub research_question_hash: String,
    pub subquestions: Vec<SubQuestion>,
}

impl DecompositionPlan {
    pub fn new(fields: DecompositionPlanFields) -> ResearchResult<Self> {
        let plan = Self {
            plan_id: fields.plan_id,
            research_question_id: fields.research_question_id,
            research_question_hash: fields.research_question_hash,
            subquestions: fields.subquestions,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("plan_id", &self.plan_id)?;
        require_non_empty("research_question_id", &self.research_question_id)?;
        require_digest("research_question_hash", &self.research_question_hash)?;
        if self.subquestions.is_empty() {
            return Err(ResearchError::validation(
                "decomposition plan requires at least one subquestion",
            ));
        }
        let mut ids = BTreeMap::new();
        for sq in &self.subquestions {
            sq.validate()?;
            if ids.insert(sq.id.clone(), ()).is_some() {
                return Err(ResearchError::validation(format!(
                    "duplicate subquestion id {}",
                    sq.id.as_str()
                )));
            }
        }
        for sq in &self.subquestions {
            for dep in &sq.depends_on {
                if !ids.contains_key(dep) {
                    return Err(ResearchError::validation(format!(
                        "subquestion {} depends on unknown {}",
                        sq.id.as_str(),
                        dep.as_str()
                    )));
                }
            }
        }
        detect_cycle(&self.subquestions)?;
        Ok(())
    }

    /// Subquestions with no unmet dependencies among `completed` ids.
    pub fn ready_subquestions<'a>(
        &'a self,
        completed: &BTreeSet<SubQuestionId>,
    ) -> Vec<&'a SubQuestion> {
        self.subquestions
            .iter()
            .filter(|sq| {
                !completed.contains(&sq.id) && sq.depends_on.iter().all(|d| completed.contains(d))
            })
            .collect()
    }
}

fn detect_cycle(subquestions: &[SubQuestion]) -> ResearchResult<()> {
    // DFS cycle detection on depends_on edges (edge: sq -> dep means sq waits
    // on dep; for cycle we treat depends_on as adjacency sq -> dep).
    let index: BTreeMap<&SubQuestionId, &SubQuestion> =
        subquestions.iter().map(|sq| (&sq.id, sq)).collect();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();

    fn dfs(
        id: &SubQuestionId,
        index: &BTreeMap<&SubQuestionId, &SubQuestion>,
        visiting: &mut BTreeSet<SubQuestionId>,
        visited: &mut BTreeSet<SubQuestionId>,
    ) -> ResearchResult<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(ResearchError::validation(format!(
                "decomposition plan has a dependency cycle involving {}",
                id.as_str()
            )));
        }
        if let Some(sq) = index.get(id) {
            for dep in &sq.depends_on {
                dfs(dep, index, visiting, visited)?;
            }
        }
        visiting.remove(id);
        visited.insert(id.clone());
        Ok(())
    }

    for sq in subquestions {
        dfs(&sq.id, &index, &mut visiting, &mut visited)?;
    }
    Ok(())
}
