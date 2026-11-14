// When a page re-reads the library. A page loads the first time it is shown,
// reloads on a change only while it is on screen, and otherwise remembers that
// it is stale and reloads the next time it is shown. Scan batches only mark
// it stale: nothing reloads per batch while a scan is running.

use std::cell::Cell;

#[derive(Debug, Default)]
pub(super) struct ReloadGate {
    mapped: Cell<bool>,
    loaded: Cell<bool>,
    stale: Cell<bool>,
}

impl ReloadGate {
    /// The page came on screen; true when it must load now.
    pub fn on_map(&self) -> bool {
        self.mapped.set(true);
        let load = !self.loaded.get() || self.stale.get();
        if load {
            self.loaded.set(true);
            self.stale.set(false);
        }
        load
    }

    pub fn on_unmap(&self) {
        self.mapped.set(false);
    }

    /// The library changed for good (scan finished, helper ready, files
    /// moved); true when the page is on screen and must reload now.
    pub fn on_change(&self) -> bool {
        if self.mapped.get() {
            self.loaded.set(true);
            self.stale.set(false);
            true
        } else {
            self.stale.set(true);
            false
        }
    }

    /// More files landed mid-scan: reload later, never now.
    pub fn mark_stale(&self) {
        self.stale.set(true);
    }

    pub fn is_mapped(&self) -> bool {
        self.mapped.get()
    }

    #[cfg(test)]
    pub fn is_stale(&self) -> bool {
        self.stale.get()
    }

    pub fn has_loaded(&self) -> bool {
        self.loaded.get()
    }

    /// The page reloaded for its own reasons (a search, an action it ran).
    pub fn loaded(&self) {
        self.loaded.set(true);
        self.stale.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_loads_on_first_show_and_not_again_on_every_show() {
        let gate = ReloadGate::default();
        assert!(!gate.has_loaded());
        assert!(gate.on_map());
        assert!(gate.has_loaded());
        gate.on_unmap();
        assert!(!gate.on_map());
    }

    #[test]
    fn a_hidden_page_never_reloads_and_catches_up_when_shown() {
        let gate = ReloadGate::default();
        assert!(!gate.on_change());
        assert!(!gate.on_change());
        assert!(gate.on_map());
        gate.on_unmap();
        assert!(!gate.on_change());
        assert!(gate.is_stale());
        assert!(gate.on_map());
        assert!(!gate.is_stale());
    }

    #[test]
    fn a_visible_page_reloads_when_a_scan_finishes_but_not_per_batch() {
        let gate = ReloadGate::default();
        assert!(gate.on_map());
        for _ in 0..500 {
            gate.mark_stale();
        }
        assert!(gate.is_mapped());
        assert!(gate.on_change());
        assert!(!gate.is_stale());
        gate.mark_stale();
        gate.on_unmap();
        assert!(gate.on_map());
    }
}
