// SPDX-License-Identifier: GPL-3.0-only
//! Page changes follow decoded video availability, rather than connection messages.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Home,
    Player,
}

#[derive(Debug, Default)]
pub struct Navigation {
    page: Page,
    open_on_first_frame: bool,
}

impl Navigation {
    pub fn page(&self) -> Page {
        self.page
    }

    /// A new connection always starts on the configuration page.
    pub fn begin_connection(&mut self) {
        self.page = Page::Home;
        self.open_on_first_frame = true;
    }

    /// Call only when a new decoded frame is available for the current session.
    /// Returns whether this frame changed the visible page.
    pub fn first_frame(&mut self) -> bool {
        if !std::mem::take(&mut self.open_on_first_frame) {
            return false;
        }
        let changed = self.page != Page::Player;
        self.page = Page::Player;
        changed
    }

    /// Returning to configuration must not be undone by the next video frame.
    pub fn show_home(&mut self) {
        self.page = Page::Home;
        self.open_on_first_frame = false;
    }

    pub fn show_player(&mut self) {
        self.page = Page::Player;
        self.open_on_first_frame = false;
    }

    /// A reconnect may arrive without an explicit new connection request.
    /// The first decoded frame of that session should open its player again.
    pub fn disconnected(&mut self) {
        self.page = Page::Home;
        self.open_on_first_frame = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{Navigation, Page};

    #[test]
    fn startup_stays_on_home_until_connection_has_a_decoded_frame() {
        let mut navigation = Navigation::default();
        assert_eq!(navigation.page(), Page::Home);
        assert!(!navigation.first_frame());
        navigation.begin_connection();
        assert_eq!(navigation.page(), Page::Home);
        assert!(navigation.first_frame());
        assert_eq!(navigation.page(), Page::Player);
        assert!(!navigation.first_frame());
    }

    #[test]
    fn returning_home_is_respected_for_the_rest_of_the_session() {
        let mut navigation = Navigation::default();
        navigation.begin_connection();
        assert!(navigation.first_frame());
        navigation.show_home();
        for _ in 0..10 {
            assert!(!navigation.first_frame());
            assert_eq!(navigation.page(), Page::Home);
        }
        navigation.show_player();
        assert_eq!(navigation.page(), Page::Player);
        assert!(!navigation.first_frame());
    }

    #[test]
    fn choosing_home_while_connecting_also_cancels_automatic_navigation() {
        let mut navigation = Navigation::default();
        navigation.begin_connection();
        navigation.show_home();
        assert!(!navigation.first_frame());
        assert_eq!(navigation.page(), Page::Home);
    }

    #[test]
    fn a_new_connection_rearms_automatic_navigation() {
        let mut navigation = Navigation::default();
        navigation.begin_connection();
        assert!(navigation.first_frame());
        navigation.show_home();
        navigation.begin_connection();
        assert_eq!(navigation.page(), Page::Home);
        assert!(navigation.first_frame());
        assert_eq!(navigation.page(), Page::Player);
    }

    #[test]
    fn disconnect_returns_home_and_reconnect_opens_on_its_first_frame() {
        let mut navigation = Navigation::default();
        navigation.begin_connection();
        assert!(navigation.first_frame());
        navigation.disconnected();
        assert_eq!(navigation.page(), Page::Home);
        assert!(navigation.first_frame());
        assert_eq!(navigation.page(), Page::Player);
        assert!(!navigation.first_frame());
    }

    #[test]
    fn manual_player_navigation_consumes_automatic_navigation() {
        let mut navigation = Navigation::default();
        navigation.begin_connection();
        navigation.show_player();
        assert!(!navigation.first_frame());
        navigation.show_home();
        assert!(!navigation.first_frame());
        assert_eq!(navigation.page(), Page::Home);
    }
}
