//! The HTTP server (librqbit's API): streams files to a media player while they download, and —
//! when turned on in Settings — serves remote control, a web interface at /web/ and the API.
//! Always behind a password: the user's for remote control, a random one otherwise.

use crate::config::Config;
use anyhow::{Context, Result};
use librqbit::Api;
use librqbit::http_api::{HttpApi, HttpApiOptions};

#[derive(Clone, Debug)]
pub struct Server {
    /// http://user:password@127.0.0.1:port — what a local player is given.
    pub local_base: String,
    pub port: u16,
    pub remote: bool,
}

impl Server {
    /// A URL that streams one file of a torrent, seeking included (HTTP ranges). The file name
    /// at the end helps players pick a demuxer.
    pub fn stream_url(&self, torrent_id: usize, file_idx: usize, file_name: &str) -> String {
        let name = file_name.rsplit('/').next().unwrap_or(file_name);
        let name = gtk::glib::Uri::escape_string(name, None, false);
        format!("{}/torrents/{torrent_id}/stream/{file_idx}/{name}", self.local_base)
    }
}

fn escape(s: &str) -> String {
    gtk::glib::Uri::escape_string(s, None, false).to_string()
}

pub async fn start(api: Api, rt: tokio::runtime::Handle, cfg: &Config) -> Result<Server> {
    let remote = cfg.remote_enabled && !cfg.remote_password.is_empty();
    let (host, port, user, password) = if remote {
        let host = if cfg.remote_lan { "0.0.0.0" } else { "127.0.0.1" };
        let user = if cfg.remote_user.is_empty() { "ferrite".to_string() } else { cfg.remote_user.clone() };
        (host, cfg.remote_port, user, cfg.remote_password.clone())
    } else {
        // Streaming only: this computer, any free port, a password nobody else knows.
        ("127.0.0.1", 0, "ferrite".to_string(), gtk::glib::uuid_string_random().to_string())
    };
    let opts = HttpApiOptions {
        read_only: !remote,
        basic_auth: Some((user.clone(), password.clone())),
        allow_create: false,
        max_upload_body_size: None,
    };
    let addr = format!("{host}:{port}");
    let port = rt
        .spawn(async move {
            use librqbit_dualstack_sockets::{BindOpts, TcpListener};
            let sock_addr: std::net::SocketAddr = addr.parse()?;
            let listener = TcpListener::bind_tcp(
                sock_addr,
                BindOpts { request_dualstack: false, reuseport: false, device: None },
            )
            .with_context(|| format!("couldn't listen on {addr}"))?;
            let port = listener.bind_addr().port();
            tokio::spawn(HttpApi::new(api, Some(opts)).make_http_api_and_run(listener, None));
            Ok::<_, anyhow::Error>(port)
        })
        .await??;
    Ok(Server {
        local_base: format!("http://{}:{}@127.0.0.1:{port}", escape(&user), escape(&password)),
        port,
        remote,
    })
}
