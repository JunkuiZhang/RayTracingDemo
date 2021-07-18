use std::{
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
};

use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    entity::obj_traits::{Hittable, HittableLight},
    settings::WINDOW_WIDTH,
    world::job_distribution::process_job_sequence,
};

pub struct ThreadPool {
    workers: Vec<Worker>,
    pub result: Receiver<Arc<(u32, [u8; (WINDOW_WIDTH * 3) as usize])>>,
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
        objects: Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
        lights: Arc<Vec<Arc<dyn HittableLight + Send + Sync>>>,
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
        self.sender.send(Message::Terminate).unwrap();
    }

    pub fn get_thread_num(&self) -> usize {
        self.workers.len()
    }
}

impl Worker {
    pub fn new(
        id: usize,
        receiver: Arc<Mutex<Receiver<Message>>>,
        res_sender: Sender<Arc<(u32, [u8; (WINDOW_WIDTH * 3) as usize])>>,
        camera: Arc<Camera>,
        objects: Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
        lights: Arc<Vec<Arc<dyn HittableLight + Send + Sync>>>,
    ) -> Self {
        let thread = thread::spawn(move || loop {
            let mut rng = ThreadRng::default();
            let c = camera.clone();
            let o = objects.clone();
            let l = lights.clone();
            let msg = receiver.lock().unwrap().recv().unwrap();
            match msg {
                Message::NewWork(work) => {
                    println!("Thread {} working..", id);
                    let res = Arc::new(process_job_sequence(work, &c, &o, &l, &mut rng));
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
