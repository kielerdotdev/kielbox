//! Asynchronous shell icon requests shared by the disk tables.
use gpui::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, mpsc},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Folder,
    OpenFolder,
    File(String),
}

type Request = (u64, Key);
type Response = (u64, Key, Option<crate::shell_icons::Icon>);

pub struct FileTypeIcons {
    request: Option<mpsc::SyncSender<Request>>,
    response: mpsc::Receiver<Response>,
    cached: HashMap<Key, Option<Arc<RenderImage>>>,
    order: VecDeque<Key>,
    pending: HashSet<Key>,
    retired: Vec<Arc<RenderImage>>,
    polling: bool,
    epoch: u64,
}

impl FileTypeIcons {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(64);
        let (done, response) = mpsc::sync_channel(64);
        let worker = std::thread::Builder::new()
            .name("shell-type-icons".into())
            .spawn(move || {
                while let Ok((epoch, key)) = rx.recv() {
                    let image = match &key {
                        Key::Folder => crate::shell_icons::load(None, false),
                        Key::OpenFolder => crate::shell_icons::load(None, true),
                        Key::File(extension) => crate::shell_icons::load(Some(extension), false)
                            .or_else(|| crate::shell_icons::load(Some(""), false)),
                    };
                    if done.send((epoch, key, image)).is_err() {
                        break;
                    }
                }
            });
        Self {
            request: worker.ok().map(|_| tx),
            response,
            cached: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
            retired: Vec::new(),
            polling: false,
            epoch: 0,
        }
    }

    pub fn get(&mut self, key: Key, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        if let Some(image) = self.cached.get(&key) {
            return image.clone();
        }
        if !self.pending.contains(&key)
            && self
                .request
                .as_ref()
                .is_some_and(|tx| tx.try_send((self.epoch, key.clone())).is_ok())
        {
            self.pending.insert(key);
        }
        if !self.pending.is_empty() && !self.polling {
            self.polling = true;
            cx.spawn(async move |view, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    if view.update(cx, |this, cx| this.poll(cx)).ok() != Some(true) {
                        break;
                    }
                }
            })
            .detach();
        }
        None
    }

    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        loop {
            match self.response.try_recv() {
                Ok((epoch, key, icon)) => {
                    if epoch != self.epoch {
                        continue;
                    }
                    self.pending.remove(&key);
                    let image = icon.and_then(|mut icon| {
                        // RenderImage consumes BGRA pixels; the shell adapter exposes RGBA.
                        for pixel in icon.rgba.as_chunks_mut::<4>().0 {
                            pixel.swap(0, 2);
                        }
                        crate::image_editor::render_image(icon.width, icon.height, icon.rgba)
                    });
                    if self.order.len() == 256
                        && let Some(old) = self.order.pop_front()
                        && let Some(Some(image)) = self.cached.remove(&old)
                    {
                        self.retired.push(image);
                    }
                    self.order.push_back(key.clone());
                    self.cached.insert(key, image);
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.request = None;
                    self.pending.clear();
                    break;
                }
            }
        }
        self.polling = !self.pending.is_empty();
        if changed {
            cx.notify();
        }
        self.polling
    }

    pub fn prepare(&mut self, window: &mut Window) {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.epoch = self.epoch.wrapping_add(1);
        self.retired
            .extend(self.cached.drain().filter_map(|(_, image)| image));
        self.cached = HashMap::new();
        self.pending = HashSet::new();
        self.order = VecDeque::new();
        cx.notify();
    }

    pub fn ready(&self) -> bool {
        self.pending.is_empty()
    }
}
