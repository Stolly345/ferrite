//! RSS feeds: every few minutes Ferrite reads each enabled feed and adds the new items whose title
//! matches the feed's filter. A feed's first reading only marks what is already there as seen, so
//! adding a busy feed doesn't download its whole back catalogue.

use crate::engine::Source;
use crate::ui::window::Window;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SEEN_CAP: usize = 5000;

thread_local! {
    static LAST_CHECK: Cell<Option<Instant>> = const { Cell::new(None) };
    static CHECKING: Cell<bool> = const { Cell::new(false) };
}

/// A downloadable item from a feed.
#[derive(Debug, PartialEq)]
pub struct FeedItem {
    /// What marks it as seen: `<feed url>\t<guid or link>`.
    pub key: String,
    pub title: String,
    /// A magnet link or an http(s) link to a .torrent.
    pub url: String,
}

fn torrent_link(url: &str, mime: Option<&str>) -> bool {
    url.starts_with("magnet:")
        || mime.is_some_and(|m| m.contains("bittorrent"))
        || url.split('?').next().is_some_and(|p| p.to_ascii_lowercase().ends_with(".torrent"))
}

/// Reads a feed and returns its items that match `filter` (case-insensitive; empty = all).
pub fn parse(feed_url: &str, xml: &[u8], filter: &str) -> anyhow::Result<Vec<FeedItem>> {
    let channel = rss::Channel::read_from(xml)?;
    let re = if filter.trim().is_empty() {
        None
    } else {
        Some(regex::RegexBuilder::new(filter).case_insensitive(true).build()?)
    };
    let mut out = Vec::new();
    for item in channel.items() {
        let title = item.title().unwrap_or_default().to_string();
        if re.as_ref().is_some_and(|r| !r.is_match(&title)) {
            continue;
        }
        // Torznab feeds (Jackett, Prowlarr) put the magnet in an attribute.
        let torznab_magnet = item
            .extensions()
            .get("torznab")
            .and_then(|t| t.get("attr"))
            .and_then(|attrs| {
                attrs.iter().find(|a| a.attrs().get("name").map(String::as_str) == Some("magneturl"))
            })
            .and_then(|a| a.attrs().get("value").cloned());
        let url = item
            .enclosure()
            .filter(|e| torrent_link(e.url(), Some(e.mime_type())))
            .map(|e| e.url().to_string())
            .or(torznab_magnet)
            .or_else(|| item.link().filter(|l| torrent_link(l, None)).map(String::from));
        let Some(url) = url else { continue };
        let id = item.guid().map(|g| g.value().to_string()).unwrap_or_else(|| url.clone());
        out.push(FeedItem { key: format!("{feed_url}\t{id}"), title, url });
    }
    Ok(out)
}

pub fn tick(win: &Rc<Window>) {
    let minutes = win.config.borrow().feed_interval_min.max(5) as u64;
    let due = LAST_CHECK.with(|l| l.get().is_none_or(|t| t.elapsed() >= Duration::from_secs(minutes * 60)));
    if due {
        check_now(win);
    }
}

pub fn check_now(win: &Rc<Window>) {
    let feeds: Vec<_> = win.config.borrow().feeds.iter().filter(|f| f.enabled).cloned().collect();
    let debug = std::env::var("FERRITE_DEBUG").is_ok();
    if debug {
        eprintln!("feeds: checking {} feed(s)", feeds.len());
    }
    if feeds.is_empty() || win.demo || CHECKING.with(|c| c.replace(true)) {
        return;
    }
    LAST_CHECK.with(|l| l.set(Some(Instant::now())));
    let Some((_, rt)) = win.engine().and_then(|e| e.api()) else {
        CHECKING.with(|c| c.set(false));
        return;
    };
    let win = win.clone();
    gtk::glib::spawn_future_local(async move {
        let Ok(client) = reqwest::Client::builder()
            .user_agent(format!("Ferrite/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
        else {
            CHECKING.with(|c| c.set(false));
            return;
        };
        for feed in feeds {
            let (client, url) = (client.clone(), feed.url.clone());
            let body: Result<anyhow::Result<Vec<u8>>, tokio::task::JoinError> = rt
                .spawn(async move {
                    let resp: reqwest::Response = client.get(&url).send().await?;
                    let resp = resp.error_for_status()?;
                    anyhow::Ok(resp.bytes().await?.to_vec())
                })
                .await;
            let items = match body {
                Ok(Ok(bytes)) => parse(&feed.url, &bytes, &feed.filter),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.into()),
            };
            if debug {
                eprintln!("feeds: {} → {:?}", feed.url, items.as_ref().map(|i| i.len()));
            }
            let items = match items {
                Ok(items) => items,
                Err(e) => {
                    eprintln!("feed {}: {e:#}", feed.url);
                    continue;
                }
            };
            let prefix = format!("{}\t", feed.url);
            let primed = win.config.borrow().feed_seen.iter().any(|k| k.starts_with(&prefix));
            for item in items {
                if win.config.borrow().feed_seen.contains(&item.key) {
                    continue;
                }
                if primed {
                    match crate::ui::dialogs::add_without_asking(&win, Source::Url(item.url.clone())).await {
                        Ok(name) => win.toast(&format!("Added “{name}” from a feed")),
                        Err(e) if e.to_string().contains("already") => {}
                        Err(e) => {
                            eprintln!("feed item {}: {e:#}", item.title);
                            continue; // try again next time
                        }
                    }
                }
                let mut cfg = win.config.borrow_mut();
                cfg.feed_seen.push(item.key);
                let over = cfg.feed_seen.len().saturating_sub(SEEN_CAP);
                cfg.feed_seen.drain(..over);
            }
            // A feed with no items yet still counts as read once.
            if !primed {
                win.config.borrow_mut().feed_seen.push(format!("{prefix}(read)"));
            }
            win.config.borrow().save();
        }
        CHECKING.with(|c| c.set(false));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
<channel><title>t</title><link>http://x</link><description>d</description>
<item><title>Show S01E01 1080p</title><guid>a1</guid>
  <enclosure url="http://x/a1.torrent" length="1" type="application/x-bittorrent"/></item>
<item><title>Show S01E01 720p</title><guid>a2</guid><link>http://x/a2.torrent</link></item>
<item><title>Indexer item 1080p</title><guid>a3</guid>
  <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:abc"/></item>
<item><title>A news post</title><link>http://x/blog</link></item>
</channel></rss>"#;

    #[test]
    fn picks_torrent_items_matching_the_filter() {
        let all = parse("F", FEED.as_bytes(), "").unwrap();
        assert_eq!(all.iter().map(|i| i.url.as_str()).collect::<Vec<_>>(), ["http://x/a1.torrent", "http://x/a2.torrent", "magnet:?xt=urn:btih:abc"]);
        let hd = parse("F", FEED.as_bytes(), "1080P").unwrap();
        assert_eq!(hd.len(), 2);
        assert_eq!(hd[0].key, "F\ta1");
    }

    #[test]
    fn bad_filter_is_an_error() {
        assert!(parse("F", FEED.as_bytes(), "(").is_err());
    }
}
