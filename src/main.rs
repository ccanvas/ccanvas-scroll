use std::{collections::LinkedList, sync::Mutex};

use libccanvas::{
    bindings::{Colour, EventVariant, Subscription},
    client::{Client, ClientConfig},
    features::common::Dimension,
};
use serde::{Deserialize, Serialize};
use tokio::{sync::OnceCell, task::JoinSet};

const REQ_TAG: &str = "!scroll-request";
const RES_TAG: &str = "!scroll-response";
const READY_TAG: &str = "!scroll-ready";

static LINE_WRAP: OnceCell<bool> = OnceCell::const_new();
static WORD_WRAP: OnceCell<bool> = OnceCell::const_new();
static MAX_ENTRIES: OnceCell<usize> = OnceCell::const_new();

static CLIENT: OnceCell<Client> = OnceCell::const_new();

#[tokio::main]
async fn main() {
    let _ = CLIENT.set(Client::new(ClientConfig::default()).await);

    LINE_WRAP
        .set(
            std::env::var("LINE_WRAP")
                .map(|val| val == "1")
                .unwrap_or(true),
        )
        .unwrap();
    WORD_WRAP
        .set(
            std::env::var("WORD_WRAP")
                .map(|val| val == "1")
                .unwrap_or(false),
        )
        .unwrap();
    MAX_ENTRIES
        .set(
            std::env::var("MAX_ENTRIES")
                .unwrap_or("".to_string())
                .parse()
                .unwrap_or(100),
        )
        .unwrap();

    let ((width, height), _) = tokio::join!(
        CLIENT.get().unwrap().term_size(),
        CLIENT.get().unwrap().subscribe_multiple(vec![
            Subscription::specific_message_tag(REQ_TAG.to_string()),
            Subscription::ScreenResize
        ])
    );

    let mut state: State = State::default();
    let mut term_size = Dimension::new(width, height);
    let mut reqests = LinkedList::new();

    CLIENT
        .get()
        .unwrap()
        .broadcast(serde_json::Value::Null, READY_TAG.to_string())
        .await;

    loop {
        let event = CLIENT.get().unwrap().recv().await;

        match event.get() {
            EventVariant::Resize { width, height } => {
                term_size = Dimension::new(*width, *height);
                render(&mut state, term_size);
                CLIENT.get().unwrap().renderall().await;
            }
            EventVariant::Message {
                content, sender, ..
            } => {
                let req: ScrollRequest = match serde_json::from_value(content.clone()) {
                    Ok(val) => val,
                    Err(_) => todo!(),
                };

                reqests.push_back(req);

                let mut res = Vec::new();
                let mut updated = false;

                while let Some(req) = reqests.pop_front() {
                    match req.content {
                        ScrollRequestVariant::AddEntry { position, entry } => {
                            if let Some(uid) = state.add(entry, position) {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::Created { uid },
                                ));
                                updated = true
                            } else {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::NotFound,
                                ));
                            }
                        }
                        ScrollRequestVariant::RemoveEntry { uid } => {
                            if state.remove(uid) {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::Removed,
                                ));
                                updated = true
                            } else {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::NotFound,
                                ));
                            }
                        }
                        ScrollRequestVariant::UpdateEntry { uid, new } => {
                            if state.update(uid, new) {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::Updated,
                                ));
                                updated = true
                            } else {
                                res.push(ScrollResponse::new(
                                    req.id,
                                    ScrollResponseVariant::NotFound,
                                ));
                            };
                        }
                        ScrollRequestVariant::Multiple { requests: to_add } => {
                            reqests.extend(to_add.into_iter());

                            res.push(ScrollResponse::new(req.id, ScrollResponseVariant::Recieved));
                        }
                    }
                }

                let mut set = JoinSet::new();

                if updated {
                    state.format(term_size.width);
                    render(&mut state, term_size);
                    set.spawn(CLIENT.get().unwrap().renderall());
                }

                match res.len() {
                    1 => {
                        let _ = set.spawn(CLIENT.get().unwrap().message(
                            sender.clone(),
                            serde_json::to_value(res.pop().unwrap()).unwrap(),
                            RES_TAG.to_string(),
                        ));
                    }
                    l if l > 1 => {
                        let _ = set.spawn(
                            CLIENT.get().unwrap().message(
                                sender.clone(),
                                serde_json::to_value(ScrollResponse::new(
                                    res[0].id,
                                    ScrollResponseVariant::Multiple {
                                        responses: res
                                            .into_iter()
                                            .map(|item| item.content)
                                            .collect(),
                                    },
                                ))
                                .unwrap(),
                                RES_TAG.to_string(),
                            ),
                        );
                    }
                    _ => {}
                }

                while set.join_next().await.is_some() {}
            }
            _ => {}
        }
    }
}

fn render(state: &mut State, term_size: Dimension) {
    CLIENT.get().unwrap().clear_all();

    if term_size.width == 0 {
        return;
    }

    if term_size.width != state.formatted_cache_width {
        state.format(term_size.width);
    }

    for (y, row) in state
        .formatted_cache
        .iter()
        .skip(
            state
                .formatted_cache
                .len()
                .saturating_sub(term_size.height as usize),
        )
        .enumerate()
    {
        let mut x = 0;
        let mut colour: Option<Colour> = None;

        for chunk in row.0.iter() {
            match chunk {
                Chunk::Colour { value } => colour = Some(*value),
                Chunk::Text { value } => {
                    if let Some(colour) = colour.as_ref() {
                        for c in value.chars() {
                            CLIENT.get().unwrap().setcharcoloured(
                                x,
                                y as u32,
                                c,
                                *colour,
                                Colour::Reset,
                            );
                            x += 1
                        }
                    } else {
                        for c in value.chars() {
                            CLIENT.get().unwrap().setchar(x, y as u32, c);
                            x += 1
                        }
                    }
                }
            }
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum Chunk {
    #[serde(rename = "colour")]
    Colour { value: Colour },
    #[serde(rename = "text")]
    Text { value: String },
}

impl Chunk {
    pub fn len(&self) -> u32 {
        match self {
            Self::Colour { .. } => 0,
            Self::Text { value } => value.len() as u32,
        }
    }

    pub fn truncate(&self, length: u32) -> Self {
        match self {
            Self::Colour { .. } => self.clone(),
            Self::Text { value } => Self::Text {
                value: {
                    let mut value = value.clone();
                    value.truncate(length as usize);
                    value
                },
            },
        }
    }

    pub fn skip(&self, length: u32) -> Self {
        match self {
            Self::Colour { .. } => self.clone(),
            Self::Text { value } => Self::Text {
                value: value.chars().skip(length as usize).collect::<String>(),
            },
        }
    }
}

#[derive(Default, Deserialize, Debug)]
struct Entry(Vec<Chunk>);

impl Entry {
    pub fn push(&mut self, chunk: Chunk) {
        self.0.push(chunk)
    }

    pub fn truncate(&self, length: u32) -> Self {
        let mut new = Entry::default();
        let mut running_length = 0;

        for chunk in self.0.iter() {
            let this_len = chunk.len();

            if running_length + this_len > length {
                new.push(chunk.truncate(length - running_length));
                break;
            }

            new.push(chunk.clone());
            running_length += this_len;
        }

        new
    }

    pub fn plain_wrap(&self, length: u32) -> Vec<Self> {
        let mut new: Vec<Self> = vec![Self::default()];
        let mut running_length = 0;
        let mut previous_colour: Option<Chunk> = None;

        let mut chunks = self.0.clone();

        let mut cursor = 0;

        while cursor < self.0.len() {
            let chunk = chunks.get(cursor).unwrap();
            let this_len = chunk.len();

            if matches!(chunk, Chunk::Colour { .. }) {
                previous_colour = Some(chunk.clone())
            }

            if running_length + this_len > length {
                new.last_mut()
                    .unwrap()
                    .push(chunk.truncate(length - running_length));

                new.push(Self::default());

                running_length = 0;

                if let Some(previous_colour) = previous_colour.clone() {
                    new.last_mut().unwrap().push(previous_colour.clone());
                }

                let new_head = chunk.skip(length - running_length);
                chunks[cursor] = new_head;

                continue;
            }

            new.last_mut().unwrap().push(chunk.clone());
            running_length += this_len;
            cursor += 1;
        }

        new
    }

    pub fn split_words(&self) -> Self {
        let mut out = Vec::new();

        for chunk in self.0.clone().into_iter() {
            match chunk {
                Chunk::Text { value } => {
                    let chunks = value.split(' ').collect::<Vec<_>>();
                    if chunks.is_empty() {
                        continue;
                    }

                    for chunk in chunks.iter().take(chunks.len() - 1) {
                        out.push(Chunk::Text {
                            value: format!("{chunk} "),
                        })
                    }

                    out.push(Chunk::Text {
                        value: chunks[chunks.len() - 1].to_string(),
                    })
                }
                _ => out.push(chunk),
            }
        }

        Self(out)
    }

    pub fn word_wrap(&self, length: u32) -> Vec<Self> {
        let mut new: Vec<Self> = vec![Self::default()];
        let mut running_length = 0;
        let mut previous_colour: Option<Chunk> = None;

        let mut chunks = self.split_words().0;

        let mut cursor = 0;

        while cursor < chunks.len() {
            let chunk = chunks.get(cursor).unwrap();
            let this_len = chunk.len();

            if matches!(chunk, Chunk::Colour { .. }) {
                previous_colour = Some(chunk.clone())
            }

            if running_length + this_len > length {
                if this_len > length {
                    new.last_mut()
                        .unwrap()
                        .push(chunk.truncate(length - running_length));

                    let new_head = chunk.skip(length - running_length);
                    chunks[cursor] = new_head;
                }

                new.push(Self::default());

                running_length = 0;

                if let Some(previous_colour) = previous_colour.clone() {
                    new.last_mut().unwrap().push(previous_colour.clone());
                }

                continue;
            }

            new.last_mut().unwrap().push(chunk.clone());
            running_length += this_len;
            cursor += 1;
        }

        new
    }
}

#[derive(Debug, Default)]
struct State {
    skip: u32,
    entries: Vec<(u32, Entry)>,
    formatted_cache: Vec<Entry>,
    formatted_cache_width: u32,
}

static UID: OnceCell<Mutex<u32>> = OnceCell::const_new_with(Mutex::new(0));

fn gen_uid() -> u32 {
    let mut id = UID.get().unwrap().lock().unwrap();
    *id += 1;
    *id
}

impl State {
    pub fn format(&mut self, width: u32) {
        if width == 0 {
            return;
        }

        self.formatted_cache_width = width;
        if !LINE_WRAP.get().unwrap() {
            self.formatted_cache = self
                .entries
                .iter()
                .map(|(_, entry)| entry.truncate(width))
                .collect();
            return;
        }

        if !WORD_WRAP.get().unwrap() {
            self.formatted_cache.clear();
            for (_, entry) in self.entries.iter() {
                self.formatted_cache.append(&mut entry.plain_wrap(width))
            }
            return;
        }

        self.formatted_cache.clear();
        for (_, entry) in self.entries.iter() {
            self.formatted_cache.append(&mut entry.word_wrap(width))
        }
    }

    pub fn add(&mut self, entry: Entry, position: ScrollPosition) -> Option<u32> {
        let mut index = position
            .eval(self.entries.len() as u32 + self.skip)
            .min(self.skip + self.entries.len() as u32 + 1);

        if index < self.skip {
            self.skip += 1;
            return None;
        }

        index -= self.skip;

        let uid = gen_uid();

        if self.entries.len() < index as usize {
            self.entries.push((uid, entry));
        } else {
            self.entries.insert(index as usize, (uid, entry));
        }

        if &self.entries.len() > MAX_ENTRIES.get().unwrap() {
            self.entries.remove(0);
            self.skip += 1;
        }

        Some(uid)
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let index = self
            .entries
            .iter()
            .position(|(entry_id, _item)| &id == entry_id);

        if let Some(index) = index {
            self.entries.remove(index);
            true
        } else {
            false
        }
    }

    pub fn update(&mut self, id: u32, new: Entry) -> bool {
        let index = self
            .entries
            .iter()
            .position(|(entry_id, _item)| &id == entry_id);

        if let Some(index) = index {
            self.entries[index].1 = new;
            true
        } else {
            false
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ScrollPosition {
    #[serde(rename = "absolute")]
    Absolute { index: u32 },
    #[serde(rename = "relative")]
    Relative { index: i32 },
}

impl ScrollPosition {
    pub fn eval(&self, cursor: u32) -> u32 {
        match self {
            Self::Absolute { index } => *index,
            Self::Relative { index } => cursor.saturating_add_signed(*index),
        }
        .min(cursor)
    }
}

#[derive(Deserialize, Debug)]
struct ScrollRequest {
    #[serde(flatten)]
    pub content: ScrollRequestVariant,
    pub id: u32,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", content = "content")]
enum ScrollRequestVariant {
    #[serde(rename = "add")]
    AddEntry {
        #[serde(flatten)]
        position: ScrollPosition,
        entry: Entry,
    },
    #[serde(rename = "remove")]
    RemoveEntry { uid: u32 },
    #[serde(rename = "update")]
    UpdateEntry { uid: u32, new: Entry },
    #[serde(rename = "multiple")]
    Multiple { requests: Vec<ScrollRequest> },
}

#[derive(Serialize)]
struct ScrollResponse {
    #[serde(flatten)]
    content: ScrollResponseVariant,
    id: u32,
}

impl ScrollResponse {
    pub fn new(id: u32, content: ScrollResponseVariant) -> Self {
        Self { content, id }
    }
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ScrollResponseVariant {
    #[serde(rename = "created")]
    Created { uid: u32 },
    #[serde(rename = "updated")]
    Updated,
    #[serde(rename = "removed")]
    Removed,
    #[serde(rename = "not found")]
    NotFound,
    #[serde(rename = "recieved")]
    Recieved,
    #[serde(rename = "multiple")]
    Multiple { responses: Vec<Self> },
}
