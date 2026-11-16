//! The window's ⌘ shortcuts, taken before any part of the window sees the key.
//!
//! iced's one-line fields (the search, the model) type the letter of a key pressed with ⌘: on macOS
//! the key comes with its text, and the field inserts any text but that of ⌘A, ⌘C, ⌘V and ⌘X. So
//! ⌘L in the search wrote an `l` there and did nothing else, and every other shortcut did the
//! same. Wrapped around the whole window, this takes such a key first, when it is one of the
//! window's shortcuts, and lets every other key through: ⌘C and the rest still reach the fields.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, keyboard};

use crate::app::{Msg, shortcut};

/// `content`, with the window's ⌘ shortcuts taken before it sees them.
pub fn chords<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    Element::new(Chords {
        content: content.into(),
    })
}

struct Chords<'a> {
    content: Element<'a, Msg>,
}

/// The message of a key that is one of the window's ⌘ shortcuts. Esc is not one: a field that
/// takes it gives the keyboard back to the input, as the window's own handling of it says.
fn chord(event: &Event) -> Option<Msg> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if !modifiers.command() || matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
        return None;
    }
    shortcut(key, *modifiers)
}

// Everything but `update` is the content's own: the tree is the content's, not one around it.
impl Widget<Msg, Theme, Renderer> for Chords<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        if let Some(msg) = chord(event) {
            shell.publish(msg);
            shell.capture_event();
            return;
        }
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
