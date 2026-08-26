pub const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArticleState {
    pub is_read: bool,
    pub is_starred: bool,
}

impl ArticleState {
    pub fn mark_read(&mut self) {
        self.is_read = true;
    }
    pub fn mark_unread(&mut self) {
        self.is_read = false;
    }
    pub fn star(&mut self) {
        self.is_starred = true;
    }
    pub fn unstar(&mut self) {
        self.is_starred = false;
    }
}

pub fn mark_all_as_read(states: &mut [ArticleState]) {
    for state in states {
        state.mark_read();
    }
}

pub fn should_expire(
    is_starred: bool,
    published_at: Option<i64>,
    inserted_at: i64,
    now: i64,
) -> bool {
    !is_starred && published_at.unwrap_or(inserted_at) < now.saturating_sub(RETENTION_SECONDS)
}
