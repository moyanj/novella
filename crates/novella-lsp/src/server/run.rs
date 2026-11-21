//! 协议接收、状态分发、语义计算分离；只提交与最新输入一致的分析结果。
use crate::{
    protocol,
    server::{Server, analysis::AnalysisJob},
};
use novella_sema::SemaResult;
use serde_json::Value;
use std::{
    io::{self, BufRead, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

enum Event {
    Message(io::Result<(Value, u64)>),
    InputEnded,
    Analyzed {
        generation: u64,
        revision: u64,
        results: Vec<SemaResult>,
    },
}

/// 接收线程只读协议帧，状态线程独占文档及输出，工作线程独占分析过程。
/// 新编辑协作取消旧计算；导航在当前版本尚未定型时返回空结果，不使用旧 span。
pub fn run<R: BufRead + Send + 'static, W: Write>(
    mut reader: R,
    writer: &mut W,
) -> io::Result<i32> {
    let (sender, receiver) = mpsc::channel();
    let revision = Arc::new(AtomicU64::new(0));
    let input_revision = Arc::clone(&revision);
    let input_sender = sender.clone();
    std::thread::spawn(move || {
        let mut versions = std::collections::HashMap::<String, i32>::new();
        loop {
            let frame = match protocol::read_message(&mut reader) {
                Ok(Some(text)) => text,
                Ok(None) => break,
                Err(error) => {
                    let _ = input_sender.send(Event::Message(Err(error)));
                    break;
                }
            };
            let message: Value = match serde_json::from_str(&frame) {
                Ok(message) => message,
                Err(error) => {
                    eprintln!("[novella-lsp] 报文不是合法 JSON：{error}");
                    continue;
                }
            };
            // 只有状态线程会接受的版本才能使旧分析过期；倒序版本不能饿死有效结果。
            let changed = match message["method"].as_str() {
                Some("textDocument/didOpen") => serde_json::from_value::<
                    lsp_types::DidOpenTextDocumentParams,
                >(message["params"].clone())
                .ok()
                .is_some_and(|params| {
                    versions.insert(
                        params.text_document.uri.as_str().to_owned(),
                        params.text_document.version,
                    );
                    true
                }),
                Some("textDocument/didChange") => serde_json::from_value::<
                    lsp_types::DidChangeTextDocumentParams,
                >(message["params"].clone())
                .ok()
                .is_some_and(|params| {
                    let uri = params.text_document.uri.as_str().to_owned();
                    if params.content_changes.is_empty()
                        || versions
                            .get(&uri)
                            .is_some_and(|version| *version >= params.text_document.version)
                    {
                        return false;
                    }
                    versions.insert(uri, params.text_document.version);
                    true
                }),
                Some("textDocument/didClose") => message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| {
                        versions.remove(uri);
                        true
                    }),
                _ => false,
            };
            let current = if changed {
                input_revision
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1)
            } else {
                input_revision.load(Ordering::Relaxed)
            };
            if input_sender
                .send(Event::Message(Ok((message, current))))
                .is_err()
            {
                return;
            }
        }
        let _ = input_sender.send(Event::InputEnded);
    });
    let (jobs, work) = mpsc::channel::<(AnalysisJob, u64)>();
    std::thread::spawn(move || {
        while let Ok(mut task) = work.recv() {
            // 只执行队列中最新的完整工程快照。
            for newer in work.try_iter() {
                task = newer;
            }
            let (job, revision) = task;
            let generation = job.generation;
            let cancelled = Arc::clone(&job.cancelled);
            let results = job.execute();
            if cancelled.load(Ordering::Relaxed) {
                continue;
            }
            if sender
                .send(Event::Analyzed {
                    generation,
                    revision,
                    results,
                })
                .is_err()
            {
                return;
            }
        }
    });
    let mut server = Server::new();
    server.background = true;
    let mut input_ended = false;
    let mut active = None::<(u64, Arc<AtomicBool>)>;
    while let Ok(event) = receiver.recv() {
        match event {
            Event::InputEnded => {
                input_ended = true;
            }
            Event::Message(message) => {
                let (message, current_revision) = message?;
                for response in server.handle(&message) {
                    protocol::write_message(writer, &response)?;
                }
                if let Some(code) = server.exit_code() {
                    return Ok(code);
                }
                if server.analysis_requested {
                    if let Some((_, cancelled)) = active.take() {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                    let cancelled = Arc::new(AtomicBool::new(false));
                    let job = server.analysis_job(Arc::clone(&cancelled));
                    active = Some((job.generation, cancelled));
                    jobs.send((job, current_revision))
                        .map_err(|_| io::Error::other("语义分析线程已退出"))?;
                }
            }
            Event::Analyzed {
                generation,
                revision: analyzed_revision,
                results,
            } => {
                if active
                    .as_ref()
                    .is_some_and(|(current, _)| *current == generation)
                {
                    active = None;
                    if generation == server.generation
                        && analyzed_revision == revision.load(Ordering::Relaxed)
                        && !server.shutdown_requested
                    {
                        server.analyses = results;
                        for response in server.diagnostic_messages() {
                            protocol::write_message(writer, &response)?;
                        }
                    }
                }
            }
        }
        if input_ended && active.is_none() {
            break;
        }
    }
    Ok(0)
}
