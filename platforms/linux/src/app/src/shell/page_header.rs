use gtk::prelude::*;

use super::{hbox, label, section_label, spacer, vbox, Page};

/// Crumb (AREA / PAGE), serif 34 title and trailing page meta over a hairline.
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
        titles.append(&section_label(&page.crumb()));
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

/// The page under its header, on the frosted page fill.
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
