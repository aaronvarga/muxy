use serde::{Deserialize, Serialize};

use crate::{AppError, AppState, DividerId, ProjectId};

/// A client-owned line in the sidebar, below a top-level project and its
/// worktrees. Dividers below the same project keep their list order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Divider {
    pub id: DividerId,
    pub after: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Keeps the gap but draws no line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transparent: bool,
    #[serde(default, skip_serializing_if = "DividerSize::is_medium")]
    pub size: DividerSize,
}

/// How much vertical space a divider takes in the sidebar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DividerSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl DividerSize {
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes a reference"
    )]
    fn is_medium(&self) -> bool {
        *self == Self::Medium
    }
}

impl AppState {
    pub fn dividers(&self) -> &[Divider] {
        &self.dividers
    }

    /// Adds a divider directly below `after`, above its other dividers.
    pub fn add_divider(&mut self, after: ProjectId) -> Result<DividerId, AppError> {
        self.divider_anchor(after)?;
        let id = DividerId::new();
        self.dividers.push(Divider {
            id,
            after,
            label: None,
            transparent: false,
            size: DividerSize::Medium,
        });
        self.place_dividers(&[id], after, true)?;
        Ok(id)
    }

    /// An empty label leaves a plain line.
    pub fn set_divider_label(&mut self, id: DividerId, label: &str) -> Result<(), AppError> {
        let label = label.trim();
        self.divider_mut(id)?.label = (!label.is_empty()).then(|| label.to_owned());
        Ok(())
    }

    pub fn set_divider_transparent(
        &mut self,
        id: DividerId,
        transparent: bool,
    ) -> Result<(), AppError> {
        self.divider_mut(id)?.transparent = transparent;
        Ok(())
    }

    pub fn set_divider_size(&mut self, id: DividerId, size: DividerSize) -> Result<(), AppError> {
        self.divider_mut(id)?.size = size;
        Ok(())
    }

    pub fn remove_divider(&mut self, id: DividerId) -> Result<(), AppError> {
        self.divider_mut(id)?;
        self.dividers.retain(|divider| divider.id != id);
        Ok(())
    }

    /// Moves `ids`, in order, below `after`: directly below it when `first`,
    /// otherwise below its other dividers.
    pub fn place_dividers(
        &mut self,
        ids: &[DividerId],
        after: ProjectId,
        first: bool,
    ) -> Result<(), AppError> {
        self.divider_anchor(after)?;
        let mut moved = Vec::with_capacity(ids.len());
        for id in ids {
            let index = self
                .dividers
                .iter()
                .position(|divider| divider.id == *id)
                .ok_or_else(|| AppError::InvalidState("unknown divider".into()))?;
            let mut divider = self.dividers.remove(index);
            divider.after = after;
            moved.push(divider);
        }
        let anchored = |divider: &Divider| divider.after == after;
        let index = if first {
            self.dividers.iter().position(anchored)
        } else {
            self.dividers
                .iter()
                .rposition(anchored)
                .map(|index| index + 1)
        }
        .unwrap_or(self.dividers.len());
        self.dividers.splice(index..index, moved);
        Ok(())
    }

    /// Keeps the dividers below `project` in place when it leaves its spot,
    /// by handing them to the top-level project above it.
    pub(crate) fn release_dividers(&mut self, project: ProjectId) {
        let ids: Vec<_> = self
            .dividers
            .iter()
            .filter(|divider| divider.after == project)
            .map(|divider| divider.id)
            .collect();
        let above = self
            .projects
            .iter()
            .take_while(|candidate| candidate.id != project)
            .filter(|candidate| candidate.parent_id.is_none())
            .last()
            .map(|candidate| candidate.id);
        if let Some(above) = above
            && !ids.is_empty()
        {
            let _ = self.place_dividers(&ids, above, false);
        }
    }

    fn divider_anchor(&self, after: ProjectId) -> Result<(), AppError> {
        let project = self.project(after).ok_or(AppError::UnknownProject(after))?;
        if project.parent_id.is_some() {
            return Err(AppError::InvalidState(
                "a divider goes below a top-level project".into(),
            ));
        }
        Ok(())
    }

    fn divider_mut(&mut self, id: DividerId) -> Result<&mut Divider, AppError> {
        self.dividers
            .iter_mut()
            .find(|divider| divider.id == id)
            .ok_or_else(|| AppError::InvalidState("unknown divider".into()))
    }
}
