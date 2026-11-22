//! Moving messages and title keys between the store and a console's or emulator's own data.

pub mod folder;
pub mod savefs;
pub mod source;

use crate::Error;
use crate::store::{Mailbox, Origin, Store};

pub use folder::{CecFolder, Delivery};
pub use source::{Source, read};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub added: usize,
    pub duplicates: usize,
    /// Messages whose signature does not match the key held for their title.
    pub refused: Vec<(String, String)>,
    /// Titles registered or given a name or key.
    pub titles: Vec<u32>,
    /// Titles whose key in the dump differs from the one already held.
    pub key_conflicts: Vec<u32>,
}

/// Copies what a dump holds into the store, keeping each message's box unless `to` overrides it.
pub fn import(store: &mut Store, src: &Source, to: Option<Mailbox>) -> Result<Imported, Error> {
    let mut out = Imported::default();
    for t in &src.titles {
        if t.name.is_none() && t.key.is_none() {
            continue;
        }
        match store.add_title(t.title, t.name.as_deref(), t.key.as_ref(), false) {
            Ok(()) => out.titles.push(t.title),
            Err(Error::KeyConflict(id)) => {
                store.add_title(t.title, t.name.as_deref(), None, false)?;
                out.key_conflicts.push(id);
            }
            Err(Error::NoRoom(_) | Error::Format(_)) if !store.titles().contains(&t.title) => {}
            Err(e) => return Err(e),
        }
    }
    for f in &src.messages {
        let mailbox = to.or(f.mailbox).unwrap_or(Mailbox::Inbox);
        let c = &f.container;
        let held = store.messages(Some(c.title()), Some(mailbox))?;
        if held
            .iter()
            .any(|s| s.container.message_id() == c.message_id())
        {
            out.duplicates += 1;
            continue;
        }
        let origin = Origin::Local(format!("imported from {}", f.path));
        match store.add_message(c, mailbox, None, Some(origin)) {
            Ok(_) => out.added += 1,
            Err(Error::Authentication) => out.refused.push((
                f.path.clone(),
                "signature does not match the title's key".into(),
            )),
            Err(e @ (Error::NoRoom(_) | Error::Format(_))) => {
                out.refused.push((f.path.clone(), e.to_string()))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
