//! One message sent to several models at once, each in a conversation of its own, side by side
//! in the list, named by its model, to compare what each does.

use iced::Task;

use crate::app::{App, Msg};

impl App {
    /// The models the message goes to: those chosen, by name.
    pub(crate) fn compared(&self) -> Vec<String> {
        self.compare.iter().cloned().collect()
    }

    /// The comparing screen's messages; any other comes back.
    pub(crate) fn update_compare(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::CompareToggle(model) => {
                if !self.compare.remove(&model) {
                    self.compare.insert(model);
                }
                Task::none()
            }
            Msg::CompareType(typed) => {
                self.compare_typed = typed;
                Task::none()
            }
            Msg::CompareAdd => {
                let model = self.compare_typed.trim().to_owned();
                if !model.is_empty() {
                    self.compare.insert(model);
                    self.compare_typed.clear();
                }
                Task::none()
            }
            Msg::CompareSend => {
                let prompt = self.chat.editor.text().trim().to_owned();
                let models = self.compared();
                if prompt.is_empty() || models.is_empty() {
                    return Ok(Task::none());
                }
                self.overlay = None;
                self.set_input("");
                let mut tasks = Vec::new();
                let mut first = None;
                for model in models {
                    let mut chat = self.new_chat();
                    chat.model = Some(model);
                    let key = chat.key;
                    self.parked.push(chat);
                    tasks.push(self.start_run(key, prompt.clone(), false));
                    first.get_or_insert(key);
                }
                // The first is shown; the others run beside it, in the list.
                if let Some(i) = first.and_then(|k| self.parked.iter().position(|c| c.key == k)) {
                    let chat = self.parked.remove(i);
                    self.bring(chat);
                }
                Task::batch(tasks)
            }
            other => return self.update_usage(other),
        };
        Ok(task)
    }
}
