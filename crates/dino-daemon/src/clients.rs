//! The clients attached to a session, and which of them its size follows.
//!
//! A session has one terminal size, and two clients can show it at two sizes: the user's Dino and
//! a second one left running (a dev build, another Mac's), or a Dino pane and `dino attach` in
//! another terminal. Whichever resized last used to win, so a client nobody was looking at (a
//! hidden window laying itself out) could set the agent's size under the one the user was looking
//! at, and the agent then drew for rows that pane doesn't have: stale lines left over, until a
//! resize there set it back.
//!
//! The size follows the client the user is looking at: the one most recently focused, as its
//! terminal reports (`dino attach` turns focus reporting on in it). The others' sizes are kept
//! and used only once one of them is focused, or is the newest left. A client attaching takes over
//! unless another one is focused right now, and gives it back if its terminal then says it isn't
//! focused (a window in the background, a hidden app's). A client whose terminal reports no focus
//! at all decides only as the newest to attach, as before.

/// What a client said about itself.
struct Client {
    id: u64,
    size: (u16, u16),
    focused: bool,
    /// It has been focused since it attached.
    was_focused: bool,
    /// When it last took over (focused, or attached with no other focused): the latest decides.
    claimed: u64,
}

#[derive(Default)]
pub(crate) struct Clients {
    list: Vec<Client>,
    clock: u64,
}

impl Clients {
    /// A client attached at `size`.
    pub(crate) fn join(&mut self, id: u64, size: (u16, u16)) {
        let claimed = if self.focused() { 0 } else { self.tick() };
        self.list.push(Client { id, size, focused: false, was_focused: false, claimed });
    }

    pub(crate) fn leave(&mut self, id: u64) {
        self.list.retain(|c| c.id != id);
    }

    pub(crate) fn resize(&mut self, id: u64, size: (u16, u16)) {
        if let Some(c) = self.list.iter_mut().find(|c| c.id == id) {
            c.size = size;
        }
    }

    /// The client's terminal gained or lost focus. Focus takes over the size; a client not
    /// focused from the start gives back what it took by attaching.
    pub(crate) fn focus(&mut self, id: u64, on: bool) {
        let now = self.tick();
        if let Some(c) = self.list.iter_mut().find(|c| c.id == id) {
            c.focused = on;
            if on {
                c.claimed = now;
                c.was_focused = true;
            } else if !c.was_focused {
                c.claimed = 0;
            }
        }
    }

    /// Whether any client's terminal is focused: what the agent is told, if it asks.
    pub(crate) fn focused(&self) -> bool {
        self.list.iter().any(|c| c.focused)
    }

    /// The size the session should be: its deciding client's. The latest to take over; among
    /// those that never did, the newest.
    pub(crate) fn size(&self) -> Option<(u16, u16)> {
        self.list.iter().max_by_key(|c| c.claimed).map(|c| c.size)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DINO: (u16, u16) = (163, 43);
    const OTHER: (u16, u16) = (150, 50);

    #[test]
    fn one_client_decides_as_before() {
        let mut c = Clients::default();
        c.join(1, DINO);
        assert_eq!(c.size(), Some(DINO));
        c.resize(1, OTHER);
        assert_eq!(c.size(), Some(OTHER));
        c.leave(1);
        assert_eq!(c.size(), None);
    }

    #[test]
    fn a_client_attaching_in_the_background_doesnt_take_the_size() {
        let mut c = Clients::default();
        c.join(1, DINO);
        c.focus(1, true);
        // A second Dino starts in the background and attaches, then lays its window out.
        c.join(2, OTHER);
        c.focus(2, false);
        c.resize(2, (140, 48));
        assert_eq!(c.size(), Some(DINO));
        // Looked at, it does.
        c.focus(1, false);
        c.focus(2, true);
        assert_eq!(c.size(), Some((140, 48)));
        // And back.
        c.focus(2, false);
        c.focus(1, true);
        assert_eq!(c.size(), Some(DINO));
    }

    #[test]
    fn the_last_focused_keeps_the_size_while_nothing_is() {
        let mut c = Clients::default();
        c.join(1, DINO);
        c.join(2, OTHER);
        c.focus(1, true);
        // The user switches to another app: Dino's pane loses focus, and the hidden other one
        // resizes.
        c.focus(1, false);
        c.resize(2, (100, 30));
        assert_eq!(c.size(), Some(DINO));
        // Their own pane resizing still counts.
        c.resize(1, (160, 40));
        assert_eq!(c.size(), Some((160, 40)));
    }

    #[test]
    fn a_client_attaching_while_none_is_focused_takes_over() {
        let mut c = Clients::default();
        c.join(1, DINO);
        c.focus(1, true);
        c.focus(1, false);
        // `dino attach` in another terminal, which reports no focus.
        c.join(2, OTHER);
        assert_eq!(c.size(), Some(OTHER));
        // Gone again: back to the one before.
        c.leave(2);
        assert_eq!(c.size(), Some(DINO));
    }

    #[test]
    fn a_client_attaching_unfocused_gives_the_size_back() {
        let mut c = Clients::default();
        c.join(1, DINO);
        c.focus(1, true);
        // The user is in another app when a second Dino starts in the background.
        c.focus(1, false);
        c.join(2, OTHER);
        c.focus(2, false);
        assert_eq!(c.size(), Some(DINO));
        // Alone, it decides all the same.
        c.leave(1);
        assert_eq!(c.size(), Some(OTHER));
    }

    #[test]
    fn the_size_goes_back_to_the_last_focused_when_the_deciding_one_leaves() {
        let mut c = Clients::default();
        c.join(1, DINO);
        c.join(2, OTHER);
        c.join(3, (90, 20));
        c.focus(2, true);
        c.focus(2, false);
        c.focus(3, true);
        c.leave(3);
        assert_eq!(c.size(), Some(OTHER));
        assert!(!c.focused());
    }
}
