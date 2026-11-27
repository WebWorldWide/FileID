use gtk::prelude::*;

use super::Page;
use crate::v3::{hbox, label, section as section_label, spacer, vbox};

#[derive(Clone)]
pub struct PageHeader {
    pub root: gtk::Box,
    meta: gtk::Label,
}

impl PageHeader {
    pub fn new(page: &Page) -> Self {
        let root = hbox(16);
        root.add_css_class("page-header");
        let titles = vbox(7);
        let crumb = section_label(&page.crumb());
        crumb.set_ellipsize(gtk::pango::EllipsizeMode::None);
        titles.append(&crumb);
        titles.append(
            &gtk::Label::builder()
                .label(page.title)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["title-1"])
                .accessible_role(gtk::AccessibleRole::Heading)
                .build(),
        );
        root.append(&titles);
        root.append(&spacer());
        let meta = label("", &["page-meta"]);
        meta.set_valign(gtk::Align::End);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        root.append(&meta);
        Self { root, meta }
    }

    pub fn set_meta(&self, text: &str) {
        if self.meta.text() != text {
            self.meta.set_text(text);
        }
    }
}

pub fn wrap(page: &Page, content: &gtk::Widget) -> (gtk::Box, PageHeader) {
    let header = PageHeader::new(page);
    let column = vbox(0);
    column.add_css_class("fileid-page");
    column.append(&header.root);
    content.set_vexpand(true);
    content.set_hexpand(true);
    column.append(content);
    (column, header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a display; run alone with XDG_DATA_HOME pointing at scratch"]
    fn the_breadcrumb_is_never_shortened() {
        if !crate::display_test::init() {
            return;
        }
        let header = PageHeader::new(super::super::page("settings").unwrap());
        let crumb = header
            .root
            .first_child()
            .and_then(|titles| titles.first_child())
            .and_downcast::<gtk::Label>()
            .expect("breadcrumb label");
        assert_eq!(crumb.text(), "FILEID / SETTINGS");
        assert_eq!(crumb.ellipsize(), gtk::pango::EllipsizeMode::None);
    }
}
