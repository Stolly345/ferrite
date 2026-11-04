//! Drawn widgets: the piece bar and the speed graph. Every colour comes from CSS through each
//! drawing area's own `color` (style.css: .piecebar, .graph-down, .graph-up, .graph-base), so the
//! palette lives in one place.

use gtk::prelude::*;
use gtk::{cairo, gdk};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

fn set_source(cr: &cairo::Context, c: &gdk::RGBA) {
    cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
}

/// A square strip on the rule: one column per group of pieces we have, in the state's colour.
#[derive(Clone)]
pub struct PieceBar {
    pub root: gtk::Box,
    area: gtk::DrawingArea,
    pieces: Rc<RefCell<Vec<bool>>>,
}

impl PieceBar {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().content_height(8).hexpand(true).build();
        area.add_css_class("piecebar");
        let root = gtk::Box::builder().hexpand(true).valign(gtk::Align::Center).css_classes(["piece-track"]).build();
        root.append(&area);
        let pieces: Rc<RefCell<Vec<bool>>> = Rc::default();
        let p = pieces.clone();
        area.set_draw_func(move |area, cr, w, h| {
            let pieces = p.borrow();
            if pieces.is_empty() {
                return;
            }
            set_source(cr, &area.color());
            let (w, h, n) = (w as f64, h as f64, pieces.len() as f64);
            // One rectangle per run of pieces we have.
            let mut start: Option<usize> = None;
            for (i, &have) in pieces.iter().chain(std::iter::once(&false)).enumerate() {
                match (have, start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        let x0 = (s as f64 / n * w).floor();
                        let x1 = (i as f64 / n * w).ceil().max(x0 + 1.0);
                        cr.rectangle(x0, 0.0, x1 - x0, h);
                        start = None;
                    }
                    _ => {}
                }
            }
            let _ = cr.fill();
        });
        PieceBar { root, area, pieces }
    }

    pub fn set(&self, pieces: Vec<bool>, state_class: &str) {
        let classes = ["piecebar", state_class];
        if self.area.css_classes().iter().map(|c| c.as_str()).ne(classes.iter().copied()) {
            self.area.set_css_classes(&classes);
        }
        if *self.pieces.borrow() != pieces {
            *self.pieces.borrow_mut() = pieces;
            self.area.queue_draw();
        }
    }
}

pub const HISTORY: usize = 120;

type Samples = Rc<RefCell<VecDeque<(u64, u64)>>>;

/// Download and upload speed over the last two minutes: three stacked layers (base, upload,
/// download), each drawing in its own CSS colour on a shared scale.
#[derive(Clone)]
pub struct SpeedGraph {
    pub root: gtk::Overlay,
    layers: [gtk::DrawingArea; 3],
    samples: Samples,
}

fn scale(samples: &VecDeque<(u64, u64)>) -> f64 {
    samples.iter().map(|&(d, u)| d.max(u)).max().unwrap_or(0).max(10_000) as f64 * 1.15
}

impl SpeedGraph {
    pub fn new() -> Self {
        let samples: Samples = Rc::default();
        let base = gtk::DrawingArea::builder().hexpand(true).vexpand(true).css_classes(["graph-base"]).build();
        base.set_draw_func(|area, cr, w, h| {
            set_source(cr, &area.color());
            cr.set_line_width(1.0);
            let y = h as f64 - 1.5;
            cr.move_to(0.0, y);
            cr.line_to(w as f64, y);
            let _ = cr.stroke();
        });
        let line = |class: &str, pick_down: bool| {
            let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).css_classes([class]).build();
            let s = samples.clone();
            area.set_draw_func(move |area, cr, w, h| {
                let samples = s.borrow();
                if samples.len() < 2 {
                    return;
                }
                let (w, h) = (w as f64, h as f64);
                let max = scale(&samples);
                let (top, bottom) = (4.0, h - 2.0);
                let step = w / (HISTORY - 1) as f64;
                let offset = (HISTORY - samples.len()) as f64 * step;
                let pts: Vec<(f64, f64)> = samples
                    .iter()
                    .enumerate()
                    .map(|(i, &(d, u))| {
                        let v = if pick_down { d } else { u } as f64;
                        (offset + i as f64 * step, bottom - v / max * (bottom - top))
                    })
                    .collect();
                set_source(cr, &area.color());
                cr.set_line_width(1.5);
                cr.set_line_join(cairo::LineJoin::Miter);
                cr.move_to(pts[0].0, pts[0].1);
                // Straight segments: the instrument draws what it measured, no smoothing.
                for p in &pts[1..] {
                    cr.line_to(p.0, p.1);
                }
                let _ = cr.stroke();
            });
            area
        };
        let up = line("graph-up", false);
        let down = line("graph-down", true);
        let root = gtk::Overlay::builder().child(&base).hexpand(true).vexpand(true).build();
        root.add_overlay(&up);
        root.add_overlay(&down);
        SpeedGraph { root, layers: [base, up, down], samples }
    }

    pub fn set(&self, samples: &VecDeque<(u64, u64)>) {
        *self.samples.borrow_mut() = samples.clone();
        for l in &self.layers {
            l.queue_draw();
        }
    }
}
