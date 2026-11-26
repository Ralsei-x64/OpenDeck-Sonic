use anyhow::{Context as _, Result, ensure};
use resvg::{
	tiny_skia::{Color, FilterQuality, IntRect, Mask, Pixmap, PixmapPaint, Transform},
	usvg,
};
use std::{
	sync::{Arc, LazyLock},
	time::{Duration, Instant},
};

pub const WIDTH: u32 = 248;
pub const HEIGHT: u32 = 58;
const CARD_W: u32 = 232;
const CARD_H: u32 = 50;
const LABEL_W: u32 = 170;
pub const SWIPE: Duration = Duration::from_millis(360);

#[derive(serde::Deserialize)]
pub struct Label {
	width: u32,
	svg: String,
}

#[derive(serde::Deserialize)]
pub struct Scene {
	pub page: String,
	pub direction: Option<i8>,
	base: Option<String>,
	rows: Option<Vec<Label>>,
	timeline: Option<String>,
}

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
	let mut db = usvg::fontdb::Database::new();
	db.load_system_fonts();
	#[cfg(target_os = "macos")]
	{
		db.set_sans_serif_family("System Font");
		db.set_monospace_family("Menlo");
	}
	Arc::new(db)
});

fn raster(fragment: &str, w: u32, h: u32, x: u32, y: u32) -> Result<Pixmap> {
	let mut pixels = Pixmap::new(w, h).context("Invalid Infobar sprite dimensions")?;
	if fragment.is_empty() {
		return Ok(pixels);
	}
	let mut opt = usvg::Options {
		fontdb: FONTS.clone(),
		font_family: "sans-serif".into(),
		..Default::default()
	};
	// Artwork and album covers travel inline. The scene has no filesystem base URL.
	opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
	let svg = format!(
		r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="{x} {y} {w} {h}" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif">{fragment}</svg>"#
	);
	let tree = usvg::Tree::from_str(&svg, &opt)?;
	resvg::render(&tree, Transform::identity(), &mut pixels.as_mut());
	Ok(pixels)
}

fn blit(dst: &mut Pixmap, src: &Pixmap, x: f32, y: f32, scale: f32) {
	let paint = PixmapPaint {
		quality: FilterQuality::Bilinear,
		..Default::default()
	};
	dst.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_row(scale, 0., 0., scale, x, y), None);
}

struct Row {
	sprite: Pixmap,
	width: u32,
}

impl Row {
	fn new(label: Label, height: u32) -> Result<Self> {
		ensure!(label.width <= 65535, "Label exceeds native sprite limit");
		let width = label.width;
		let sprite = raster(&label.svg, width.max(1), height, 0, 0)?;
		if width <= LABEL_W {
			return Ok(Self { sprite, width });
		}
		// Append the gap and the beginning of the next copy once, including one
		// extra sample for interpolation at the right edge of the viewport.
		let mut looped = Pixmap::new(width + 24 + LABEL_W + 1, height).unwrap();
		blit(&mut looped, &sprite, 0., 0., 1.);
		blit(&mut looped, &sprite, (width + 24) as f32, 0., 1.);
		Ok(Self { sprite: looped, width })
	}

	fn scroll_into(&self, frame: &mut Pixmap, y: usize, offset: f64) {
		// tiny-skia rounds pure translations to whole pixels. Interpolate in
		// premultiplied RGBA so the 23 px/s marquee can move at 30 or 60 fps.
		let first = offset.floor() as usize % (self.width as usize + 24);
		let weight = (offset.fract() * 256.).round() as u32;
		let span = LABEL_W as usize * 4;
		for (line, src) in self.sprite.data().chunks_exact(self.sprite.width() as usize * 4).enumerate() {
			let start = ((y + line) * WIDTH as usize + 61) * 4;
			let dst = &mut frame.data_mut()[start..start + span];
			let samples = &src[first * 4..first * 4 + span + 4];
			for (pixel, pair) in dst.chunks_exact_mut(4).zip(samples.windows(8).step_by(4)) {
				let sample = |c: usize| (u32::from(pair[c]) * (256 - weight) + u32::from(pair[c + 4]) * weight + 128) >> 8;
				let alpha = sample(3);
				if alpha == 0 {
					continue;
				}
				for (c, channel) in pixel[..3].iter_mut().enumerate() {
					*channel = (sample(c) + (u32::from(*channel) * (255 - alpha) + 127) / 255) as u8;
				}
			}
		}
	}
}

struct Page {
	name: String,
	base: Pixmap,
	background: Pixmap,
	rows: Vec<Row>,
	timeline: Option<Pixmap>,
	labels_since: Instant,
}

impl Page {
	fn new(name: String, now: Instant) -> Self {
		Self {
			name,
			base: Pixmap::new(CARD_W, CARD_H).unwrap(),
			background: Pixmap::new(WIDTH, HEIGHT).unwrap(),
			rows: Vec::new(),
			timeline: None,
			labels_since: now,
		}
	}

	fn update(&mut self, scene: Scene, now: Instant) -> Result<()> {
		if let Some(art) = scene.base {
			self.base = raster(&art, CARD_W, CARD_H, 0, 0)?;
		}
		if let Some(labels) = scene.rows {
			ensure!(labels.len() <= 2, "Invalid media rows");
			let mut rows = Vec::new();
			for (i, label) in labels.into_iter().enumerate() {
				rows.push(Row::new(label, if i == 0 { 20 } else { 12 })?);
			}
			self.rows = rows;
			self.labels_since = now;
		}
		if let Some(art) = scene.timeline {
			self.timeline = if art.is_empty() { None } else { Some(raster(&art, LABEL_W, 18, 53, 32)?) };
		}
		// Static art, timestamps and fitting labels are composited only on a content change.
		self.background.fill(Color::BLACK);
		blit(&mut self.background, &self.base, 8., 4., 1.);
		if let Some(timeline) = &self.timeline {
			blit(&mut self.background, timeline, 61., 36., 1.);
		}
		for (i, row) in self.rows.iter().enumerate() {
			if row.width <= LABEL_W {
				blit(&mut self.background, &row.sprite, 61., if i == 0 { 4. } else { 24. }, 1.);
			}
		}

		Ok(())
	}

	fn label_age(&self, now: Instant) -> f64 {
		now.saturating_duration_since(self.labels_since).as_secs_f64()
	}

	fn next_scroll(&self, now: Instant, interval: Duration) -> Option<Duration> {
		let age = self.label_age(now);
		self.rows
			.iter()
			.filter(|row| row.width > LABEL_W)
			.map(|row| {
				let phase = age % (1.4 + (row.width + 24) as f64 / 23.);
				if phase < 1.4 { Duration::from_secs_f64(1.4 - phase).max(interval) } else { interval }
			})
			.min()
	}

	fn compose(&self, now: Instant) -> Pixmap {
		let mut page = self.background.clone();
		for (i, row) in self.rows.iter().enumerate() {
			if row.width <= LABEL_W {
				continue;
			}
			let y = if i == 0 { 4 } else { 24 };
			let stride = (row.width + 24) as f64;
			let phase = self.label_age(now) % (1.4 + stride / 23.);
			row.scroll_into(&mut page, y, (phase - 1.4).max(0.) * 23.);
		}
		page
	}
}

static ROUNDED: LazyLock<Mask> = LazyLock::new(|| {
	let mut mask = Mask::new(CARD_W, CARD_H).unwrap();
	for (i, alpha) in mask.data_mut().iter_mut().enumerate() {
		let x = (i as u32 % CARD_W) as f32 + 0.5;
		let y = (i as u32 / CARD_W) as f32 + 0.5;
		let dx = x - x.clamp(10., CARD_W as f32 - 10.);
		let dy = y - y.clamp(10., CARD_H as f32 - 10.);
		*alpha = ((10.5 - dx.hypot(dy)).clamp(0., 1.) * 255.).round() as u8;
	}
	mask
});

fn rounded(frame: Pixmap) -> Pixmap {
	let mut card = frame.clone_rect(IntRect::from_xywh(8, 4, CARD_W, CARD_H).unwrap()).unwrap();
	card.apply_mask(&ROUNDED);
	card
}
fn ease(t: f32) -> f32 {
	let t = t.clamp(0., 1.);
	(1. - (1. + 7. * t) * (-7. * t).exp()) / (1. - 8. * (-7f32).exp())
}

struct Transition {
	from: Pixmap,
	to: Pixmap,
	start: Instant,
	dir: f32,
}

pub struct Renderer {
	page: Page,
	swipe: Option<Transition>,
}

impl Renderer {
	pub fn new(now: Instant) -> Self {
		Self {
			page: Page::new(String::new(), now),
			swipe: None,
		}
	}

	pub fn update(&mut self, scene: Scene, now: Instant) -> Result<()> {
		if let Some(dir) = scene.direction {
			ensure!((-1..=1).contains(&dir), "Invalid swipe direction");
			let mut page = Page::new(scene.page.clone(), now);
			page.update(scene, now)?;
			self.swipe = (dir != 0).then(|| Transition {
				from: rounded(self.page.compose(now)),
				to: rounded(page.compose(now)),
				start: now,
				dir: dir as f32,
			});
			self.page = page;
		} else {
			ensure!(scene.page == self.page.name, "Infobar patch addressed a different page");
			self.page.update(scene, now)?;
			if let Some(swipe) = &mut self.swipe {
				swipe.to = rounded(self.page.compose(now));
			}
		}
		Ok(())
	}

	pub fn render(&mut self, now: Instant) -> Pixmap {
		if self.swipe.as_ref().is_some_and(|s| now.duration_since(s.start) >= SWIPE) {
			self.swipe = None;
			self.page.labels_since = now;
		}
		let card = if let Some(s) = &self.swipe {
			let t = now.duration_since(s.start).as_secs_f32() / SWIPE.as_secs_f32();
			let x = -s.dir * 236. * ease(t);
			let mut view = Pixmap::new(CARD_W, CARD_H).unwrap();
			for (sprite, dx, scale) in [(&s.from, x, 1. - 0.18 * ease(t / 0.24)), (&s.to, x + s.dir * 236., 0.82 + 0.18 * ease((t - 0.45) / 0.55))] {
				blit(&mut view, sprite, dx + 116. * (1. - scale), 25. * (1. - scale), scale);
			}
			view
		} else {
			return self.page.compose(now);
		};
		let mut frame = Pixmap::new(WIDTH, HEIGHT).unwrap();
		frame.fill(Color::BLACK);
		blit(&mut frame, &card, 8., 4., 1.);
		frame
	}

	pub fn media_partial(&self) -> bool {
		self.page.name == "media" && self.swipe.is_none()
	}
	pub fn next(&self, now: Instant, interval: Duration) -> Option<Duration> {
		if let Some(s) = &self.swipe {
			return Some(interval.min((s.start + SWIPE).saturating_duration_since(now)));
		}
		self.page.next_scroll(now, interval)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
	pub x: u32,
	pub y: u32,
	pub w: u32,
	pub h: u32,
}
pub const FULL: Rect = Rect { x: 0, y: 0, w: WIDTH, h: HEIGHT };

pub fn damage(before: Option<&Pixmap>, after: &Pixmap, partial: bool) -> Option<Rect> {
	let Some(before) = before else { return Some(FULL) };
	if before.data() == after.data() {
		return None;
	}
	if !partial {
		return Some(FULL);
	}
	let (mut left, mut top, mut right, mut bottom) = (WIDTH, HEIGHT, 0, 0);
	for (i, (a, b)) in before.pixels().iter().zip(after.pixels()).enumerate() {
		if a != b {
			let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
			left = left.min(x);
			top = top.min(y);
			right = right.max(x);
			bottom = bottom.max(y);
		}
	}
	Some(Rect {
		x: left,
		y: top,
		w: right - left + 1,
		h: bottom - top + 1,
	})
}

impl Rect {
	// The JPEG encoder processes 8 px blocks starting at the rotated top-left.
	// Cover whole blocks so a partial write decodes like the same area of a full frame.
	pub fn jpeg_blocks(self) -> Self {
		fn align(start: u32, length: u32, limit: u32) -> (u32, u32) {
			let end = limit - (limit - start - length) / 8 * 8;
			let start = limit.saturating_sub((limit - start).next_multiple_of(8));
			(start, end - start)
		}
		let (x, w) = align(self.x, self.w, WIDTH);
		let (y, h) = align(self.y, self.h, HEIGHT);
		Self { x, y, w, h }
	}
}

pub fn encode(frame: &Pixmap, rect: Rect) -> Result<Vec<u8>> {
	// The Neo takes logical rectangle coordinates and a locally rotated JPEG.
	let mut rgb = Vec::with_capacity((rect.w * rect.h * 3) as usize);
	for y in (rect.y..rect.y + rect.h).rev() {
		for x in (rect.x..rect.x + rect.w).rev() {
			let i = ((y * WIDTH + x) * 4) as usize;
			rgb.extend_from_slice(&frame.data()[i..i + 3]);
		}
	}
	let mut bytes = Vec::new();
	image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90).encode(&rgb, rect.w, rect.h, image::ExtendedColorType::Rgb8)?;
	Ok(bytes)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn patch(page: &str, direction: i8, colour: &str) -> Scene {
		Scene {
			page: page.into(),
			direction: Some(direction),
			base: Some(format!(r#"<rect width="232" height="50" fill="{colour}"/>"#)),
			rows: None,
			timeline: None,
		}
	}
	fn rgb(image: &Pixmap, x: u32, y: u32) -> &[u8] {
		let i = ((y * WIDTH + x) * 4) as usize;
		&image.data()[i..i + 3]
	}

	#[test]
	fn swipe_scales_translates_clips_and_settles_in_both_directions() {
		for dir in [-1, 1] {
			let start = Instant::now();
			let mut r = Renderer::new(start);
			r.update(patch("clock", 0, "red"), start).unwrap();
			r.update(patch("media", dir, "blue"), start).unwrap();
			let early = r.render(start + Duration::from_millis(36));
			let middle = r.render(start + Duration::from_millis(140));
			let end = r.render(start + SWIPE);
			assert_eq!(rgb(&early, 124, 29), [255, 0, 0]);
			assert_eq!(rgb(&middle, 124, 29), [0, 0, 255]);
			assert_eq!(rgb(&middle, 124, 5), [0, 0, 0]);
			assert_eq!(rgb(&end, 124, 5), [0, 0, 255]);
			assert_eq!(rgb(&end, 4, 29), [0, 0, 0]);
			assert!(r.next(start + SWIPE, Duration::from_millis(17)).is_none());
		}
	}

	#[test]
	fn marquee_and_timeline_damage_preserve_album_pixels() {
		let start = Instant::now();
		let mut r = Renderer::new(start);
		let mut scene = patch("media", 0, "black");
		scene.rows = Some(vec![Label {
			width: 220,
			svg: r#"<rect width="80" height="10" fill="red"/>"#.into(),
		}]);
		r.update(scene, start).unwrap();
		let a = r.render(start);
		let fractional = r.render(start + Duration::from_millis(1410));
		assert!((180..210).contains(&rgb(&fractional, 140, 7)[0]));
		let b = r.render(start + Duration::from_millis(1750));
		let rect = damage(Some(&a), &b, true).unwrap();
		assert!(rect.x >= 61 && rect.x + rect.w <= 231 && rect.y >= 4 && rect.y + rect.h <= 22);
		let time = start + Duration::from_millis(1750);
		r.update(
			Scene {
				page: "media".into(),
				direction: None,
				base: None,
				rows: None,
				timeline: Some(r#"<rect x="90" y="35" width="50" height="5" fill="white"/>"#.into()),
			},
			time,
		)
		.unwrap();
		let c = r.render(time);
		assert_eq!(damage(Some(&b), &c, true), Some(Rect { x: 98, y: 39, w: 50, h: 5 }));
		assert_eq!(damage(Some(&c), &c, true), None);
		let mut replay = a.clone();
		for (source, rect) in [(&b, rect), (&c, damage(Some(&b), &c, true).unwrap())] {
			for y in rect.y..rect.y + rect.h {
				let i = ((y * WIDTH + rect.x) * 4) as usize;
				let j = i + 4 * rect.w as usize;
				replay.data_mut()[i..j].copy_from_slice(&source.data()[i..j]);
			}
		}
		assert_eq!(replay.data(), c.data());
	}

	#[test]
	fn decoded_patch_matches_full_jpeg_including_screen_edges() {
		let mut view = Pixmap::new(WIDTH, HEIGHT).unwrap();
		for (i, px) in view.data_mut().chunks_exact_mut(4).enumerate() {
			px.copy_from_slice(&[(i % 239) as u8, (i * 3 % 255) as u8, (i * 13 % 255) as u8, 255]);
		}
		let decode = |rect| image::load_from_memory(&encode(&view, rect).unwrap()).unwrap().to_rgb8();
		let full = decode(FULL);
		for rect in [Rect { x: 61, y: 8, w: 170, h: 12 }, Rect { x: 3, y: 1, w: 5, h: 6 }, Rect { x: 229, y: 45, w: 19, h: 13 }] {
			let r = rect.jpeg_blocks();
			let patch = decode(r);
			for (x, y, pixel) in patch.enumerate_pixels() {
				assert_eq!(pixel, full.get_pixel(WIDTH - r.x - r.w + x, HEIGHT - r.y - r.h + y));
			}
		}
	}

	#[test]
	fn jpeg_rotates_the_patch_around_its_own_center() {
		let mut frame = Pixmap::new(WIDTH, HEIGHT).unwrap();
		for y in 10..26 {
			for x in 20..52 {
				let i = ((y * WIDTH + x) * 4) as usize;
				frame.data_mut()[i..i + 4].copy_from_slice(if x < 36 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
			}
		}
		let jpeg = encode(&frame, Rect { x: 20, y: 10, w: 32, h: 16 }).unwrap();
		let out = image::load_from_memory(&jpeg).unwrap().to_rgb8();
		assert_eq!(out.dimensions(), (32, 16));
		assert!(out.get_pixel(3, 8)[2] > 245 && out.get_pixel(29, 8)[0] > 245);
	}
}
