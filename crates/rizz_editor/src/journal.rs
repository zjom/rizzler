//! Append-only ring-buffer history of user-visible messages and commands.
//! Surfaced through `:messages` / `:history`.

use rizz_ringbuffer::RingBuffer;
use std::rc::Rc;

const MESSAGE_CAPACITY: usize = 200;
const COMMAND_CAPACITY: usize = 2000;

#[derive(Default)]
pub struct Journal {
    messages: RingBuffer<Rc<str>, MESSAGE_CAPACITY>,
    commands: RingBuffer<Rc<str>, COMMAND_CAPACITY>,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_message(&mut self, msg: &str) {
        self.messages.push_back(msg.into());
    }

    pub fn messages(&self) -> impl Iterator<Item = &Rc<str>> {
        self.messages.iter()
    }

    pub fn record_command(&mut self, cmd: &str) {
        self.commands.push_back(cmd.into());
    }

    pub fn commands(&self) -> impl Iterator<Item = &Rc<str>> {
        self.commands.iter()
    }

    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// The recorded command at `idx` counting from the oldest (front), or
    /// `None` if out of range. Backs `<up>`/`<down>` history recall.
    pub fn command(&self, idx: usize) -> Option<&Rc<str>> {
        self.commands.iter().nth(idx)
    }
}
