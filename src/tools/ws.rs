//! WebSocket 测试客户端：连接在调用方提供的 tokio 运行时上，收发经 flume 通道，界面异步等待事件。
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Debug, PartialEq)]
pub enum WsEvent { Open, Text(String), Binary(usize), Closed(String), Error(String) }

/// 发送端：`Some(文本)` 发送一条消息，`None` 关闭连接。
pub type WsSender = flume::Sender<Option<String>>;

pub fn connect(rt: &tokio::runtime::Handle, url: String) -> (WsSender, flume::Receiver<WsEvent>) {
    let (tx, rx) = flume::unbounded::<Option<String>>();
    let (etx, erx) = flume::unbounded();
    rt.spawn(async move {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => { let _ = etx.send(WsEvent::Error(e.to_string())); return; }
        };
        let _ = etx.send(WsEvent::Open);
        let (mut sink, mut stream) = ws.split();
        loop {
            tokio::select! {
                out = rx.recv_async() => match out {
                    Ok(Some(text)) => if let Err(e) = sink.send(Message::Text(text.into())).await { let _ = etx.send(WsEvent::Error(e.to_string())); break; },
                    _ => { let _ = sink.send(Message::Close(None)).await; let _ = etx.send(WsEvent::Closed(String::new())); break; }
                },
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(s))) => { let _ = etx.send(WsEvent::Text(s.to_string())); }
                    Some(Ok(Message::Binary(b))) => { let _ = etx.send(WsEvent::Binary(b.len())); }
                    Some(Ok(Message::Close(f))) => { let _ = etx.send(WsEvent::Closed(f.map(|f| format!("{} {}", u16::from(f.code), f.reason)).unwrap_or_default())); break; }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { let _ = etx.send(WsEvent::Error(e.to_string())); break; }
                    None => { let _ = etx.send(WsEvent::Closed(String::new())); break; }
                },
            }
        }
    });
    (tx, erx)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bad_url_reports_error() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (_tx, rx) = super::connect(rt.handle(), "not a url".into());
        let ev = rt.block_on(async { rx.recv_async().await.unwrap() });
        assert!(matches!(ev, super::WsEvent::Error(_)));
    }
}
