// Library multi-select, the parts that aren't widgets: what the Selected pane
// shows for a selection and which grid positions to re-select after a reload
// reorders or replaces the rows. Clicks and keys use GtkMultiSelection's own
// rules (click selects one, Ctrl+click toggles, Shift+click and Shift+arrows
// extend from the anchor).

use std::collections::HashSet;

use fileid_engine::util::display::file_size_i64;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PaneView {
    Hidden,
    One(u32),
    Many { count: usize, bytes: i64 },
}

/// `selected` is `(position, size in bytes)` for every selected tile.
pub(super) fn view(selected: &[(u32, i64)]) -> PaneView {
    match selected {
        [] => PaneView::Hidden,
        [(position, _)] => PaneView::One(*position),
        many => PaneView::Many {
            count: many.len(),
            bytes: many.iter().map(|(_, size)| (*size).max(0)).sum(),
        },
    }
}

pub(super) fn many_title(count: usize) -> String {
    format!("{} files", crate::shell::grouped(count as u64))
}

pub(super) fn many_detail(bytes: i64) -> String {
    format!("{} in all", file_size_i64(bytes))
}

/// Positions in the new rows of the files that were selected before; files
/// the reload no longer lists drop out.
pub(super) fn restore(selected: &HashSet<i64>, rows: impl IntoIterator<Item = i64>) -> Vec<u32> {
    if selected.is_empty() {
        return Vec::new();
    }
    rows.into_iter()
        .enumerate()
        .filter(|(_, id)| selected.contains(id))
        .filter_map(|(position, _)| u32::try_from(position).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_shows_one_file_or_a_count_and_combined_size() {
        assert_eq!(view(&[]), PaneView::Hidden);
        assert_eq!(view(&[(7, 100)]), PaneView::One(7));
        assert_eq!(
            view(&[(1, 1_000), (4, 2_500), (9, -1)]),
            PaneView::Many { count: 3, bytes: 3_500 }
        );
        assert_eq!(many_title(1_204), "1,204 files");
        assert_eq!(many_detail(0), format!("{} in all", file_size_i64(0)));
    }

    #[test]
    fn a_reload_keeps_the_same_files_selected_at_their_new_positions() {
        let selected: HashSet<i64> = [10, 30, 99].into_iter().collect();
        assert_eq!(restore(&selected, [30, 20, 10, 40]), vec![0, 2]);
        assert_eq!(restore(&selected, [1, 2, 3]), Vec::<u32>::new());
        assert_eq!(restore(&HashSet::new(), [10, 30]), Vec::<u32>::new());
    }

    #[test]
    fn paging_appends_without_moving_earlier_selections() {
        let selected: HashSet<i64> = [2, 5].into_iter().collect();
        let first_page = restore(&selected, [1, 2, 3]);
        let both_pages = restore(&selected, [1, 2, 3, 4, 5, 6]);
        assert_eq!(first_page, vec![1]);
        assert_eq!(both_pages, vec![1, 4]);
    }
}
