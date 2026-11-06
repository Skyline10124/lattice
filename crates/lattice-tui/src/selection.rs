// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_api::{
    Effort, ModelCandidate, ModelRef,
    local::{SessionHead, TurnId},
};

struct Chosen<T> {
    value: T,
    generation: u64,
}

pub(crate) struct SelectedModel {
    pub(crate) model: ModelRef,
    pub(crate) supported_efforts: Vec<Effort>,
}

#[derive(Default)]
pub(crate) struct Selection {
    model: Option<Chosen<SelectedModel>>,
    effort: Option<Chosen<Effort>>,
    generation: u64,
    /// 请求正文由 LocalTurn 持有；这里只按发送顺序记录字段代次，避免旧接受消费重选。
    sent: Vec<(TurnId, Option<u64>, Option<u64>)>,
}

impl Selection {
    pub(crate) fn model(&self) -> Option<&ModelRef> {
        self.model.as_ref().map(|choice| &choice.value.model)
    }

    pub(crate) fn choose_model(&mut self, candidate: ModelCandidate) {
        let model = ModelRef::new(candidate.provider, candidate.model);
        self.generation += 1;
        self.model = Some(Chosen {
            value: SelectedModel {
                model,
                supported_efforts: candidate.supported_efforts,
            },
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
        match &self.model {
            Some(choice)
                if choice.value.model.provider().as_str() != head.provider
                    || choice.value.model.model().as_str() != head.model =>
            {
                [head.effort, head.profile_default_effort]
                    .into_iter()
                    .flatten()
                    .find(|effort| choice.value.supported_efforts.contains(effort))
            }
            _ => head.effort,
        }
    }

    pub(crate) fn submission(&mut self, turn: TurnId) -> (Option<ModelRef>, Option<Effort>) {
        self.sent.push((
            turn,
            self.model.as_ref().map(|choice| choice.generation),
            self.effort.as_ref().map(|choice| choice.generation),
        ));
        (
            self.model().cloned(),
            self.effort.as_ref().map(|choice| choice.value),
        )
    }

    pub(crate) fn accepted(&mut self, turn: TurnId) {
        if let Some(index) = self.sent.iter().position(|(id, ..)| *id == turn) {
            let (_, model, effort) = self.sent[index];
            // 较新请求包含当时全部意图；接受后，更早的代次已经消费或被重选。
            self.sent.drain(..=index);
            self.model
                .take_if(|choice| Some(choice.generation) == model);
            self.effort
                .take_if(|choice| Some(choice.generation) == effort);
        }
    }

    pub(crate) fn rejected(&mut self, turn: TurnId) {
        self.sent.retain(|(id, ..)| *id != turn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_api::{
        Availability, ModelId, ProviderId,
        local::{NodeId, SessionId},
    };

    fn head(effort: Option<Effort>) -> SessionHead {
        SessionHead {
            session_id: SessionId::parse("11111111111141118111111111111111").unwrap(),
            position: NodeId(0),
            agent: "agent".into(),
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
            provider: ProviderId::parse(provider).unwrap(),
            model: ModelId::parse(model).unwrap(),
            name: None,
            context: None,
            output: None,
            supported_efforts: efforts.to_vec(),
            cost: None,
            availability: Availability::Ready,
        }
    }

    #[test]
    fn model_fallback_is_only_a_preview_and_same_model_keeps_none() {
        let mut selection = Selection::default();
        let head = head(Some(Effort::High));
        selection.choose_model(candidate("p", "b", &[Effort::Low]));
        assert_eq!(selection.preview_effort(Some(&head)), Some(Effort::Low));
        let turn = lattice_api::local::new_turn_id();
        assert_eq!(
            selection.submission(turn),
            (Some(ModelRef::parse("p/b").unwrap()), None)
        );
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
            (Some(ModelRef::parse("p/keep").unwrap()), Some(Effort::Max))
        );
    }
}
