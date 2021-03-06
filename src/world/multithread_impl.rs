use std::{
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, RwLock,
    },
    thread,
};

use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    data::{RowColGBuffer, RowColPixels},
    entity::obj_traits::{Hittable, HittableLight},
    world::job_distribution::process_job_sequence,
};

pub struct ThreadPool {
    workers: Vec<Worker>,
    pub result: Receiver<Arc<(u32, RowColPixels, RowColGBuffer)>>,
    sender: Sender<Message>,
}

struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
}

pub enum Message {
    NewWork(u32),
    Terminate,
}

impl ThreadPool {
    pub fn new(
        size: usize,
        camera: Arc<Camera>,
        objects: Arc<RwLock<Vec<Arc<dyn Hittable + Send + Sync>>>>,
        lights: Arc<RwLock<Vec<Arc<dyn HittableLight + Send + Sync>>>>,
    ) -> Self {
        let mut workers = Vec::with_capacity(size);
        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let (r_sender, r_receiver) = mpsc::channel();
        for id in 0..size {
            workers.push(Worker::new(
                id,
                Arc::clone(&receiver),
                r_sender.clone(),
                camera.clone(),
                objects.clone(),
                lights.clone(),
            ));
        }
        return ThreadPool {
            workers,
            sender,
            result: r_receiver,
        };
    }

    pub fn work(&self, w: u32) {
        // let job = Box::new(w);
        self.sender.send(Message::NewWork(w)).unwrap();
    }

    pub fn shut_down(&self) {
        for _ in 0..self.workers.len() {
            self.sender.send(Message::Terminate).unwrap();
        }
    }
}

impl Worker {
    pub fn new(
        id: usize,
        receiver: Arc<Mutex<Receiver<Message>>>,
        res_sender: Sender<Arc<(u32, RowColPixels, RowColGBuffer)>>,
        camera: Arc<Camera>,
        objects: Arc<RwLock<Vec<Arc<dyn Hittable + Send + Sync>>>>,
        lights: Arc<RwLock<Vec<Arc<dyn HittableLight + Send + Sync>>>>,
    ) -> Self {
        let thread = thread::spawn(move || loop {
            let mut rng = ThreadRng::default();
            let o = objects.read().unwrap();
            let l = lights.read().unwrap();
            let msg = receiver.lock().unwrap().recv().unwrap();
            match msg {
                Message::NewWork(work) => {
                    let res =
                        Arc::new(process_job_sequence(work, camera.clone(), &o, &l, &mut rng));
                    res_sender.send(res).unwrap();
                }
                Message::Terminate => {
                    println!("Thread {} was told to shut down..", id);
                    break;
                }
            }
        });
        return Worker {
            id,
            thread: Some(thread),
        };
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        for worker in self.workers.iter_mut() {
            println!("=> Worker {} shutting down.", worker.id);
            if let Some(thread) = worker.thread.take() {
                thread.join().unwrap();
            }
        }
    }
}
