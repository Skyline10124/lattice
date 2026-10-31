// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use lattice_api::{
    Effort, ModelCandidate,
    local::{SessionHead, TurnId},
};

struct Chosen<T> {
    value: T,
    generation: u64,
}

#[derive(Default)]
pub(crate) struct Selection {
    model: Option<Chosen<ModelCandidate>>,
    effort: Option<Chosen<Effort>>,
    generation: u64,
    /// 请求正文由 LocalTurn 持有；这里只配对发送时的字段代次，避免旧接受消费重选。
    sent: HashMap<TurnId, (Option<u64>, Option<u64>)>,
}

impl Selection {
    pub(crate) fn model(&self) -> Option<&ModelCandidate> {
        self.model.as_ref().map(|choice| &choice.value)
    }

    pub(crate) fn choose_model(&mut self, model: ModelCandidate) {
        self.generation += 1;
        self.model = Some(Chosen {
            value: model,
            generation: self.generation,
        });
    }

    pub(crate) fn choose_effort(&mut self, effort: Effort) {
        self.generation += 1;
        self.effort = Some(Chosen {
            value: effort,
            generation: self.generation,
        });
    }

    pub(crate) fn preview_effort(&self, head: Option<&SessionHead>) -> Option<Effort> {
        if let Some(effort) = &self.effort {
            return Some(effort.value);
        }
        let head = head?;
        match self.model() {
            Some(model) if model.provider != head.provider || model.model != head.model => {
                [head.effort, head.profile_default_effort]
                    .into_iter()
                    .flatten()
                    .find(|effort| model.supported_efforts.contains(effort))
            }
            _ => head.effort,
        }
    }

    pub(crate) fn submission(&mut self, turn: TurnId) -> (Option<String>, Option<Effort>) {
        self.sent.insert(
            turn,
            (
                self.model.as_ref().map(|choice| choice.generation),
                self.effort.as_ref().map(|choice| choice.generation),
            ),
        );
        (
            self.model()
                .map(|model| format!("{}/{}", model.provider, model.model)),
            self.effort.as_ref().map(|choice| choice.value),
        )
    }

    pub(crate) fn accepted(&mut self, turn: TurnId) {
        if let Some((model, effort)) = self.sent.remove(&turn) {
            self.model
                .take_if(|choice| Some(choice.generation) == model);
            self.effort
                .take_if(|choice| Some(choice.generation) == effort);
        }
    }

    pub(crate) fn rejected(&mut self, turn: TurnId) {
        self.sent.remove(&turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_api::local::{AuthState, NodeId, SessionId};

    fn head(effort: Option<Effort>) -> SessionHead {
        SessionHead {
            session_id: SessionId::parse("11111111111141118111111111111111").unwrap(),
            position: NodeId(0),
            agent: Some("agent".into()),
            model: "a".into(),
            provider: "p".into(),
            effort,
            profile_default_effort: Some(Effort::Low),
            turn_id: None,
            owner: false,
            diagnostic: None,
            path_replaced: false,
        }
    }

    fn candidate(provider: &str, model: &str, efforts: &[Effort]) -> ModelCandidate {
        ModelCandidate {
            provider: provider.into(),
            model: model.into(),
            name: None,
            context: None,
            output: None,
            supported_efforts: efforts.to_vec(),
            cost: None,
            credential_status: AuthState::Ready,
        }
    }

    #[test]
    fn model_fallback_is_only_a_preview_and_same_model_keeps_none() {
        let mut selection = Selection::default();
        let head = head(Some(Effort::High));
        selection.choose_model(candidate("p", "b", &[Effort::Low]));
        assert_eq!(selection.preview_effort(Some(&head)), Some(Effort::Low));
        let turn = lattice_api::local::new_turn_id();
        assert_eq!(selection.submission(turn), (Some("p/b".into()), None));
        selection.choose_model(candidate("p", "c", &[]));
        assert_eq!(selection.preview_effort(Some(&head)), None);
        selection.choose_model(candidate("p", "a", &[Effort::Low]));
        assert_eq!(
            selection.preview_effort(Some(&super::tests::head(None))),
            None
        );
        assert_eq!(selection.preview_effort(None), None);
        selection.choose_model(candidate("other", "a", &[Effort::Low]));
        assert_eq!(selection.preview_effort(Some(&head)), Some(Effort::Low));
        selection.choose_model(candidate("p", "keep", &[Effort::High]));
        assert_eq!(selection.preview_effort(Some(&head)), Some(Effort::High));
        selection.choose_effort(Effort::Max);
        assert_eq!(selection.preview_effort(Some(&head)), Some(Effort::Max));
        assert_eq!(
            selection.submission(lattice_api::local::new_turn_id()),
            (Some("p/keep".into()), Some(Effort::Max))
        );
    }
}
