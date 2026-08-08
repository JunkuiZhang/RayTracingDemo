use std::{
    sync::{
        Arc, Mutex, RwLock,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

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

#[derive(Clone)]
struct WorkerContext {
    camera: Arc<Camera>,
    objects: Arc<RwLock<Vec<Arc<dyn Hittable + Send + Sync>>>>,
    lights: Arc<RwLock<Vec<Arc<dyn HittableLight + Send + Sync>>>>,
    samples_per_pixel: usize,
    seed: u64,
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
        samples_per_pixel: usize,
        seed: u64,
    ) -> Self {
        let mut workers = Vec::with_capacity(size);
        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let (r_sender, r_receiver) = mpsc::channel();
        let context = WorkerContext {
            camera,
            objects,
            lights,
            samples_per_pixel,
            seed,
        };
        for id in 0..size {
            workers.push(Worker::new(
                id,
                Arc::clone(&receiver),
                r_sender.clone(),
                context.clone(),
            ));
        }
        ThreadPool {
            workers,
            sender,
            result: r_receiver,
        }
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
        context: WorkerContext,
    ) -> Self {
        let thread = thread::spawn(move || {
            loop {
                let o = context.objects.read().unwrap();
                let l = context.lights.read().unwrap();
                let msg = receiver.lock().unwrap().recv().unwrap();
                match msg {
                    Message::NewWork(work) => {
                        let res = Arc::new(process_job_sequence(
                            work,
                            context.camera.clone(),
                            &o,
                            &l,
                            context.samples_per_pixel,
                            context.seed,
                        ));
                        res_sender.send(res).unwrap();
                    }
                    Message::Terminate => {
                        println!("Thread {} was told to shut down..", id);
                        break;
                    }
                }
            }
        });
        Worker {
            id,
            thread: Some(thread),
        }
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
