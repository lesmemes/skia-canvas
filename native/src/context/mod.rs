#![allow(unused_mut)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]
use std::rc::Rc;
use std::cell::RefCell;
use std::ops::Range;
use neon::prelude::*;
use neon::object::This;
use neon::result::Throw;
use skia_safe::{Canvas as SkCanvas, Surface, Paint, Path, PathOp, Image, ImageInfo,
                Matrix, Rect, Point, IPoint, Size, ISize, Color, Color4f, ColorType,
                PaintStyle, BlendMode, FilterQuality, AlphaType, TileMode, ClipOp,
                image_filters, color_filters, table_color_filter, dash_path_effect,
                Data, PictureRecorder, Picture, Drawable};
use skia_safe::textlayout::{Paragraph, ParagraphBuilder, ParagraphStyle, TextStyle, TextShadow, RectHeightStyle, RectWidthStyle};
use skia_safe::canvas::SrcRectConstraint::Strict;
use skia_safe::path::FillType;

use crate::utils::*;
use crate::typography::*;
use crate::gradient::{CanvasGradient, JsCanvasGradient};
use crate::pattern::{CanvasPattern, JsCanvasPattern};

const BLACK:Color = Color::BLACK;
const TRANSPARENT:Color = Color::TRANSPARENT;
const GALLEY:f32 = 100_000.0;

pub mod class;
pub use class::JsContext2D;

pub struct Context2D{
  bounds: Rect,
  recorder: RefCell<PictureRecorder>,
  library: Rc<RefCell<FontLibrary>>,
  state: State,
  stack: Vec<State>,
  path: Path,
}

#[derive(Clone)]
pub struct State{
  clip: Path,
  matrix: Matrix,
  paint: Paint,

  fill_style: Dye,
  stroke_style: Dye,
  shadow_blur: f32,
  shadow_color: Color,
  shadow_offset: Point,

  stroke_width: f32,
  line_dash_offset: f32,
  line_dash_list: Vec<f32>,

  global_alpha: f32,
  global_composite_operation: BlendMode,
  image_filter_quality: FilterQuality,
  image_smoothing_enabled: bool,
  filter:String,

  font: String,
  font_variant: String,
  font_features: Vec<String>,
  char_style: TextStyle,
  graf_style: ParagraphStyle,
  text_baseline: Baseline,
  text_tracking: i32,
  text_wrap: bool,
}

impl Default for State {
  fn default() -> Self {
    let mut paint = Paint::default();
    paint
      .set_stroke_miter(10.0)
      .set_color(BLACK)
      .set_anti_alias(true)
      .set_stroke_width(1.0)
      .set_style(PaintStyle::Fill)
      .set_filter_quality(FilterQuality::Low);

    let graf_style = ParagraphStyle::new();
    let mut char_style = TextStyle::new();
    char_style.set_font_size(10.0);

    State {
      clip: Path::new(),
      matrix: Matrix::new_identity(),

      paint,
      stroke_style: Dye::Color(BLACK),
      fill_style: Dye::Color(BLACK),
      stroke_width: 1.0,
      line_dash_offset: 0.0,
      line_dash_list: vec![],

      global_alpha: 1.0,
      global_composite_operation: BlendMode::SrcOver,
      image_filter_quality: FilterQuality::Low,
      image_smoothing_enabled: true,
      filter: "none".to_string(),

      shadow_blur: 0.0,
      shadow_color: TRANSPARENT,
      shadow_offset: (0.0, 0.0).into(),

      font: "10px sans-serif".to_string(),
      font_variant: "normal".to_string(),
      font_features:vec![],
      char_style,
      graf_style,
      text_baseline: Baseline::Alphabetic,
      text_tracking: 0,
      text_wrap: false
    }
  }
}

impl Context2D{
  pub fn new(bounds: Rect, library: &Rc<RefCell<FontLibrary>>) -> Self {
    let mut recorder = PictureRecorder::new();
    recorder.begin_recording(bounds, None, None);
    if let Some(canvas) = recorder.recording_canvas() {
      canvas.save(); // start at depth 2
    }


    Context2D{
      bounds,
      recorder: RefCell::new(recorder),
      library: Rc::clone(&library),
      path: Path::new(),
      stack: vec![],
      state: State::default(),
    }
  }

  pub fn in_local_coordinates(&mut self, x: f32, y: f32) -> Point{
    match self.state.matrix.invert(){
      Some(inverse) => inverse.map_point((x, y)),
      None => (x, y).into()
    }
  }

  pub fn width(&self) -> f32{
    self.bounds.width()
  }

  pub fn height(&self) -> f32{
    self.bounds.height()
  }

  pub fn with_canvas<F>(&self, f:F)
    where F:FnOnce(&mut SkCanvas)
  {
    let mut recorder = self.recorder.borrow_mut();
    if let Some(canvas) = recorder.recording_canvas() {
      f(canvas);
    }
  }

  pub fn render_to_canvas<F>(&self, paint:&Paint, f:F)
    where F:Fn(&mut SkCanvas, &Paint)
  {
    match self.state.global_composite_operation{
      BlendMode::SrcIn | BlendMode::SrcOut |
      BlendMode::DstIn | BlendMode::DstOut |
      BlendMode::DstATop | BlendMode::Src =>{
        // for blend modes that affect regions of the canvas outside of the bounds of the object
        // being drawn, create an intermediate picture before drawing to the canvas
        let mut layer_paint = self.state.paint.clone();
        layer_paint.set_blend_mode(BlendMode::SrcOver);
        let mut layer_recorder = PictureRecorder::new();
        layer_recorder.begin_recording(self.bounds, None, None);
        if let Some(layer) = layer_recorder.recording_canvas() {
          // draw the dropshadow (if applicable)
          if let Some(shadow_paint) = self.paint_for_shadow(&layer_paint){
            layer.save();
            layer.set_matrix(&Matrix::translate(self.state.shadow_offset));
            layer.concat(&self.state.matrix);
            f(layer, &shadow_paint);
            layer.restore();
          }

          // draw normally
          layer.set_matrix(&self.state.matrix);
          f(layer, &layer_paint);
        }


        // transfer the picture contents to the canvas in a single operation, applying the blend
        // mode to the whole canvas (regardless of the bounds of the text/path being drawn)
        if let Some(pict) = layer_recorder.finish_recording_as_picture(Some(&self.bounds)){
          let mut recorder = self.recorder.borrow_mut();
          if let Some(canvas) = recorder.recording_canvas() {
            canvas.save();
            canvas.set_matrix(&Matrix::new_identity());
            canvas.draw_picture(&pict, None, Some(&paint));
            canvas.restore();
          }
        }

      },
      _ => {
        let mut recorder = self.recorder.borrow_mut();
        if let Some(canvas) = recorder.recording_canvas() {
          // only call the closure if there's an active dropshadow
          if let Some(shadow_paint) = self.paint_for_shadow(&paint){
            canvas.save();
            canvas.set_matrix(&Matrix::translate(self.state.shadow_offset));
            canvas.concat(&self.state.matrix);
            f(canvas, &shadow_paint);
            canvas.restore();
          }

          // draw with the normal paint
          f(canvas, &paint);
        }

      }
    };

  }

  pub fn with_matrix<F>(&mut self, f:F)
    where F:FnOnce(&mut Matrix) -> &Matrix
  {
    f(&mut self.state.matrix);
    self.with_canvas(|canvas| {
      canvas.set_matrix(&self.state.matrix);
    });
  }

  pub fn map_points(&self, coords:&[f32]) -> Vec<Point>{
    coords.chunks(2)
          .map(|pair| self.state.matrix.map_xy(pair[0], pair[1]))
          .collect()
  }

  pub fn reset_canvas(&mut self){
    // clears the active clip and transform from the canvas (but not from the state struct)
    self.with_canvas(|canvas|{
      canvas.restore_to_count(1);
      canvas.save();
    });
  }

  pub fn resize(&mut self, dims: impl Into<Size>) {
    // called by the canvas when .width or .height are assigned to
    self.bounds = Rect::from_size(dims);
    self.path = Path::new();
    self.stack = vec![];
    self.state = State::default();

    // erase any existing content
    let mut new_recorder = PictureRecorder::new();
    new_recorder.begin_recording(self.bounds, None, None);
    self.recorder.replace(new_recorder);
    self.reset_canvas();
  }

  pub fn push(&mut self){
    let new_state = self.state.clone();
    self.stack.push(new_state);
  }

  pub fn pop(&mut self){
    // don't do anything if we're already back at the initial stack frame
    if let Some(old_state) = self.stack.pop(){
      self.state = old_state;

      self.reset_canvas();
      self.with_canvas(|canvas|{
        canvas.set_matrix(&self.state.matrix);
        if !self.state.clip.is_empty(){
          canvas.clip_path(&self.state.clip, ClipOp::Intersect, true /* antialias */);
        }
      });
    }
  }

  pub fn draw_path(&mut self, paint: &Paint){
    // the current path has already incorporated its transform state
    let inverse = self.state.matrix.invert().unwrap();
    let path = self.path.with_transform(&inverse);

    self.render_to_canvas(&paint, |canvas, paint| {
      canvas.draw_path(&path, &paint);
    });
  }

  pub fn clip_path(&mut self, path: Option<Path>, rule:FillType){
    let mut clip = match path{
      Some(path) => path,
      None => {
        let inverse = self.state.matrix.invert().unwrap();
        self.path.with_transform(&inverse)
      }
    };

    clip.set_fill_type(rule);
    if self.state.clip.is_empty(){
      self.state.clip = clip.clone();
    }else if let Some(new_clip) = self.state.clip.op(&clip, PathOp::Intersect){
      self.state.clip = new_clip;
    }

    let do_aa = true;
    self.with_canvas(|canvas| {
      canvas.clip_path(&clip, ClipOp::Intersect, do_aa);
    });
  }

  pub fn hit_test_path(&mut self, path: &mut Path, point:impl Into<Point>, rule:Option<FillType>, style: PaintStyle) -> bool {
    let point = point.into();
    let point = self.in_local_coordinates(point.x, point.y);
    let rule = rule.unwrap_or(FillType::Winding);
    let prev_rule = path.fill_type();
    path.set_fill_type(rule);

    let is_in = match style{
      PaintStyle::Stroke => {
        let paint = self.paint_for_stroke();
        let precision = 0.3; // this is what Chrome uses to compute this
        match paint.get_fill_path(&path, None, Some(precision)){
          Some(traced_path) => traced_path.contains(point),
          None => path.contains(point)
        }
      },
      _ => path.contains(point)
    };

    path.set_fill_type(prev_rule);
    is_in
  }


  pub fn draw_rect(&mut self, rect:&Rect, paint: &Paint){
    self.render_to_canvas(&paint, |canvas, paint| {
      canvas.draw_rect(&rect, &paint);
    });
  }

  pub fn clear_rect(&mut self, rect:&Rect){
    self.with_canvas(|canvas| {
      let mut paint = Paint::default();
      paint.set_style(PaintStyle::Fill);
      paint.set_blend_mode(BlendMode::Clear);
      canvas.draw_rect(&rect, &paint);
    });
  }

  pub fn draw_drawable(&mut self, drobble:&mut Option<Drawable>, src_rect:&Rect, dst_rect:&Rect){
    let mut paint = self.state.paint.clone();
    paint.set_color(self.color_with_alpha(&BLACK));

    if let Some(mut drobble) = drobble.as_mut(){
      self.push();
      self.with_canvas(|canvas| {
        let size = ISize::new(dst_rect.width() as i32, dst_rect.height() as i32);
        let mag = Point::new(dst_rect.width()/src_rect.width(), dst_rect.height()/src_rect.height());
        let mut matrix = Matrix::new_identity();
        matrix.pre_scale( (mag.x, mag.y), None )
              .pre_translate((-src_rect.x(), -src_rect.y()));

        if let Some(shadow_paint) = self.paint_for_shadow(&paint){
          if let Some(mut surface) = Surface::new_raster_n32_premul(size){
            surface.canvas().draw_drawable(&mut drobble, Some(&matrix));
            canvas.draw_image(&surface.image_snapshot(), (dst_rect.x(), dst_rect.y()), Some(&shadow_paint));
          }
        }

        matrix.pre_translate((dst_rect.x()/mag.x, dst_rect.y()/mag.y));
        canvas.clip_rect(dst_rect, ClipOp::Intersect, true)
              .draw_drawable(&mut drobble, Some(&matrix));
      });
      self.pop();
    }
  }

  pub fn draw_image(&mut self, img:&Option<Image>, src_rect:&Rect, dst_rect:&Rect){
    let mut canvas_paint = self.state.paint.clone();
    canvas_paint
      .set_alpha_f(self.state.global_alpha);

    if let Some(image) = &img {
      self.render_to_canvas(&canvas_paint, |canvas, paint| {
        canvas.draw_image_rect(&image, Some((src_rect, Strict)), dst_rect, &paint);
      });
    }
  }

  pub fn get_drawable(&mut self) -> Option<Drawable> {
    // stop the recorder to take a snapshot then restart it again
    let mut recorder = self.recorder.borrow_mut();
    let mut drobble = recorder.finish_recording_as_drawable();
    recorder.begin_recording(self.bounds, None, None);

    if let Some(canvas) = recorder.recording_canvas() {
      // fill the newly restarted recorder with the snapshot content...
      if let Some(mut palimpsest) = drobble.as_mut() {
        canvas.draw_drawable(&mut palimpsest, None);
      }

      // ...and the current ctm/clip state
      canvas.save();
      canvas.set_matrix(&self.state.matrix);
      if !self.state.clip.is_empty(){
        canvas.clip_path(&self.state.clip, ClipOp::Intersect, true /* antialias */);
      }
    }


    drobble
  }


  pub fn get_picture(&mut self, cull: Option<&Rect>) -> Option<Picture> {
    // stop the recorder to take a snapshot then restart it again
    let mut recorder = self.recorder.borrow_mut();
    let snapshot = recorder.finish_recording_as_picture(cull.or(Some(&self.bounds)));
    recorder.begin_recording(self.bounds, None, None);

    if let Some(canvas) = recorder.recording_canvas() {
      // fill the newly restarted recorder with the snapshot content...
      if let Some(palimpsest) = &snapshot {
        canvas.draw_picture(&palimpsest, None, None);
      }

      // ...and the current ctm/clip state
      canvas.save();
      canvas.set_matrix(&self.state.matrix);
      if !self.state.clip.is_empty(){
        canvas.clip_path(&self.state.clip, ClipOp::Intersect, true /* antialias */);
      }
    }
    snapshot
  }

  pub fn get_pixels(&mut self, buffer: &mut [u8], origin: impl Into<IPoint>, size: impl Into<ISize>){
    let origin = origin.into();
    let size = size.into();
    let info = ImageInfo::new(size, ColorType::RGBA8888, AlphaType::Unpremul, None);

    if let Some(pict) = self.get_picture(None) {
      if let Some(mut bitmap_surface) = Surface::new_raster_n32_premul(size){
        let shift = Matrix::translate((-origin.x as f32, -origin.y as f32));
        bitmap_surface.canvas().draw_picture(&pict, Some(&shift), None);
        bitmap_surface.read_pixels(&info, buffer, info.min_row_bytes(), (0,0));
      }
    }
  }

  pub fn blit_pixels(&mut self, buffer: &[u8], info: &ImageInfo, src_rect:&Rect, dst_rect:&Rect){
    // works just like draw_image in terms of src/dst rects, but clears the dst_rect and then draws
    // without clips, transforms, alpha, blend, or shadows
    let data = unsafe{ Data::new_bytes(buffer) };
    if let Some(bitmap) = Image::from_raster_data(&info, data, info.min_row_bytes()) {
      self.push();
      self.reset_canvas();
      self.with_canvas(|canvas| {
        let mut paint = Paint::default();
        let mut eraser = Paint::default();
        eraser.set_blend_mode(BlendMode::Clear);
        canvas.draw_image_rect(&bitmap, Some((src_rect, Strict)), dst_rect, &eraser);
        canvas.draw_image_rect(&bitmap, Some((src_rect, Strict)), dst_rect, &paint);
      });
      self.pop();
    }
  }

  pub fn set_font(&mut self, spec: FontSpec){
    let mut library = self.library.borrow_mut();
    if let Some(new_style) = library.update_style(&self.state.char_style, &spec){
      self.state.font = spec.canonical;
      self.state.font_variant = spec.variant.to_string();
      self.state.char_style = new_style;
    }
  }

  pub fn set_font_variant(&mut self, variant:&str, features:&[(String, i32)]){
    let mut library = self.library.borrow_mut();
    let new_style = library.update_features(&self.state.char_style, features);
    self.state.font_variant = variant.to_string();
    self.state.char_style = new_style;
  }

  pub fn typeset(&mut self, text: &str, width:f32, paint: Paint) -> Paragraph {
    let mut char_style = self.state.char_style.clone();
    char_style.set_foreground_color(Some(paint));

    let mut graf_style = self.state.graf_style.clone();
    let text = match self.state.text_wrap{
      true => text.to_string(),
      false => {
        graf_style.set_max_lines(1);
        text.replace("\n", " ")
      }
    };

    let mut library = self.library.borrow_mut();
    let collection = library.collect_fonts(&char_style);
    let mut paragraph_builder = ParagraphBuilder::new(&graf_style, collection);
    paragraph_builder.push_style(&char_style);
    paragraph_builder.add_text(&text);

    let mut paragraph = paragraph_builder.build();
    paragraph.layout(width);
    paragraph
  }

  pub fn draw_text(&mut self, text: &str, x: f32, y: f32, width: Option<f32>, paint: Paint){
    let width = width.unwrap_or(GALLEY);

    let mut text_paint = paint.clone();
    text_paint.set_blend_mode(BlendMode::SrcOver);

    let mut paragraph = self.typeset(&text, width, text_paint);
    let mut point = Point::new(x, y);
    let metrics = self.state.char_style.font_metrics();
    let offset = get_baseline_offset(&metrics, self.state.text_baseline);
    point.y += offset - paragraph.alphabetic_baseline();
    point.x += width * get_alignment_factor(&self.state.graf_style);

    let mut bounds = paragraph.get_rects_for_range(0..text.len(), RectHeightStyle::IncludeLineSpacingBottom, RectWidthStyle::Tight)
      .iter().map(|textbox| textbox.rect)
      .fold(Rect::new_empty(), Rect::join2);
    bounds.outset((paint.stroke_width(), paint.stroke_width()));

    // render the text once into a picture we can use for the shadow as well
    let mut recorder = PictureRecorder::new();
    recorder.begin_recording(bounds, None, None);
    if let Some(canvas) = recorder.recording_canvas() {
      paragraph.paint(canvas, (0.0,0.0));

      if let Some(pict) = recorder.finish_recording_as_picture(Some(&bounds)){
        let position = Matrix::translate(point);
        self.render_to_canvas(&paint, |canvas, paint| {
          canvas.draw_picture(&pict, Some(&position), Some(&paint));
        });
      }
    }

  }

  pub fn measure_text(&mut self, text: &str, width:Option<f32>) -> Vec<Vec<f32>>{
    let paint = self.paint_for_fill();
    let mut paragraph = self.typeset(&text, width.unwrap_or(GALLEY), paint);

    let font_metrics = self.state.char_style.font_metrics();
    let offset = get_baseline_offset(&font_metrics, self.state.text_baseline);
    let hang = get_baseline_offset(&font_metrics, Baseline::Hanging) - offset;
    let norm = get_baseline_offset(&font_metrics, Baseline::Alphabetic) - offset;
    let ideo = get_baseline_offset(&font_metrics, Baseline::Ideographic) - offset;
    let ascent = norm - font_metrics.ascent;
    let descent = font_metrics.descent - norm;
    let alignment = get_alignment_factor(&self.state.graf_style);

    if paragraph.line_number() == 0 {
      return vec![vec![0.0, 0.0, 0.0, 0.0, 0.0, ascent, descent, ascent, descent, hang, norm, ideo]]
    }

    // find the bounds and text-range for each individual line
    let origin = paragraph.get_line_metrics()[0].baseline;
    let line_rects:Vec<(Rect, Range<usize>, f32)> = paragraph.get_line_metrics().iter().map(|line|{
      let baseline = line.baseline - origin;
      let rect = Rect::new(line.left as f32, (baseline - line.ascent) as f32,
                          (line.width - line.left) as f32, (baseline + line.descent) as f32);
      let range = string_idx_range(text, line.start_index, line.end_excluding_whitespaces);
      (rect.with_offset((alignment*rect.width(), offset)), range, baseline as f32)
    }).collect();

    // take their union to find the bounds for the whole text run
    let (bounds, chars) = line_rects.iter().fold((Rect::new_empty(), 0), |(union, indices), (rect, range, _)|
      (Rect::join2(union, rect), range.end)
    );

    // return a list-of-lists whose first entry is the whole-run font metrics and subsequent entries are
    // line-rect/range values (with the js side responsible for restructuring the whole bundle)
    let mut results = vec![vec![
      bounds.width(), bounds.left, bounds.right, -bounds.top, bounds.bottom,
      ascent, descent, ascent, descent, hang, norm, ideo
    ]];
    line_rects.iter().for_each(|(rect, range, baseline)|{
      results.push(vec![rect.left, rect.top, rect.width(), rect.height(),
                        *baseline, range.start as f32, range.end as f32])
    });
    results
  }

  pub fn set_filter(&mut self, filter_text:&str, specs:&[FilterSpec]){
    // matrices and formulæ taken from: https://www.w3.org/TR/filter-effects-1/
    let filter = specs.iter().fold(None, |chain, next_filter|
      match next_filter {
        FilterSpec::Shadow{ offset, blur, color } => {
          let sigma = *blur / 2.0;
          image_filters::drop_shadow(*offset, (sigma, sigma), *color, chain, None)
        },
        FilterSpec::Plain{ name, value } => match name.as_ref() {
          "blur" => {
            image_filters::blur((*value, *value), TileMode::Clamp, chain, None)
          },
          "brightness" => {
            let amt = value.max(0.0);
            let color_matrix = color_filters::matrix_row_major(&[
              amt,  0.0,  0.0,  0.0, 0.0,
              0.0,  amt,  0.0,  0.0, 0.0,
              0.0,  0.0,  amt,  0.0, 0.0,
              0.0,  0.0,  0.0,  1.0, 0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          "contrast" => {
            let amt = value.max(0.0);
            let mut ramp = [0u8; 256];
            for (i, val) in ramp.iter_mut().take(256).enumerate() {
              let orig = i as f32;
              *val = (127.0 + amt * orig - (127.0 * amt )) as u8;
            }
            let table = Some(&ramp);
            let color_table = table_color_filter::from_argb(None, table, table, table);
            image_filters::color_filter(color_table, chain, None)
          },
          "grayscale" => {
            let amt = 1.0 - value.max(0.0).min(1.0);
            let color_matrix = color_filters::matrix_row_major(&[
              (0.2126 + 0.7874 * amt), (0.7152 - 0.7152  * amt), (0.0722 - 0.0722 * amt), 0.0, 0.0,
              (0.2126 - 0.2126 * amt), (0.7152 + 0.2848  * amt), (0.0722 - 0.0722 * amt), 0.0, 0.0,
              (0.2126 - 0.2126 * amt), (0.7152 - 0.7152  * amt), (0.0722 + 0.9278 * amt), 0.0, 0.0,
               0.0,                     0.0,                      0.0,                    1.0, 0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          "invert" => {
            let amt = value.max(0.0).min(1.0);
            let mut ramp = [0u8; 256];
            for (i, val) in ramp.iter_mut().take(256).enumerate().map(|(i,v)| (i as f32, v)) {
              let (orig, inv) = (i, 255.0-i);
              *val = (orig * (1.0 - amt) + inv * amt) as u8;
            }
            let table = Some(&ramp);
            let color_table = table_color_filter::from_argb(None, table, table, table);
            image_filters::color_filter(color_table, chain, None)
          },
          "opacity" => {
            let amt = value.max(0.0).min(1.0);
            let color_matrix = color_filters::matrix_row_major(&[
              1.0,  0.0,  0.0,  0.0,  0.0,
              0.0,  1.0,  0.0,  0.0,  0.0,
              0.0,  0.0,  1.0,  0.0,  0.0,
              0.0,  0.0,  0.0,  amt,  0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          "saturate" => {
            let amt = value.max(0.0);
            let color_matrix = color_filters::matrix_row_major(&[
              (0.2126 + 0.7874 * amt), (0.7152 - 0.7152 * amt), (0.0722 - 0.0722 * amt), 0.0, 0.0,
              (0.2126 - 0.2126 * amt), (0.7152 + 0.2848 * amt), (0.0722 - 0.0722 * amt), 0.0, 0.0,
              (0.2126 - 0.2126 * amt), (0.7152 - 0.7152 * amt), (0.0722 + 0.9278 * amt), 0.0, 0.0,
               0.0,                     0.0,                     0.0,                    1.0, 0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          "sepia" => {
            let amt = 1.0 - value.max(0.0).min(1.0);
            let color_matrix = color_filters::matrix_row_major(&[
              (0.393 + 0.607 * amt), (0.769 - 0.769 * amt), (0.189 - 0.189 * amt), 0.0, 0.0,
              (0.349 - 0.349 * amt), (0.686 + 0.314 * amt), (0.168 - 0.168 * amt), 0.0, 0.0,
              (0.272 - 0.272 * amt), (0.534 - 0.534 * amt), (0.131 + 0.869 * amt), 0.0, 0.0,
               0.0,                   0.0,                   0.0,                  1.0, 0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          "hue-rotate" => {
            let cos = to_radians(*value).cos();
            let sin = to_radians(*value).sin();
            let color_matrix = color_filters::matrix_row_major(&[
              (0.213 + cos*0.787 - sin*0.213), (0.715 - cos*0.715 - sin*0.715), (0.072 - cos*0.072 + sin*0.928), 0.0, 0.0,
              (0.213 - cos*0.213 + sin*0.143), (0.715 + cos*0.285 + sin*0.140), (0.072 - cos*0.072 - sin*0.283), 0.0, 0.0,
              (0.213 - cos*0.213 - sin*0.787), (0.715 - cos*0.715 + sin*0.715), (0.072 + cos*0.928 + sin*0.072), 0.0, 0.0,
               0.0,                             0.0,                             0.0,                            1.0, 0.0
            ]);
            image_filters::color_filter(color_matrix, chain, None)
          },
          _ => chain
        }
      }
    );

    self.state.paint.set_image_filter(filter);
    self.state.filter = filter_text.to_string();
  }

  pub fn update_image_quality(&mut self){
    self.state.paint.set_filter_quality(match self.state.image_smoothing_enabled{
      true => self.state.image_filter_quality,
      false => FilterQuality::None
    });
  }

  pub fn color_with_alpha(&self, src:&Color) -> Color{
    let mut color:Color4f = src.clone().into();
    color.a *= self.state.global_alpha;
    color.to_color()
  }

  pub fn paint_for_fill(&self) -> Paint{
    let mut paint = self.state.paint.clone();

    let dye = &self.state.fill_style;
    let alpha = self.state.global_alpha;
    dye.mix_into(&mut paint, alpha);

    paint
  }

  pub fn paint_for_stroke(&self) -> Paint{
    let mut paint = self.state.paint.clone();
    paint.set_style(PaintStyle::Stroke);

    let dye = &self.state.stroke_style;
    let alpha = self.state.global_alpha;
    dye.mix_into(&mut paint, alpha);

    if !self.state.line_dash_list.is_empty() {
      let dash = dash_path_effect::new(&self.state.line_dash_list, self.state.line_dash_offset);
      paint.set_path_effect(dash);
    }

    paint
  }

  pub fn paint_for_shadow(&self, base_paint:&Paint) -> Option<Paint> {
    let State {shadow_color, shadow_blur, shadow_offset, ..} = self.state;
    let sigma = shadow_blur / 2.0;

    match shadow_color.a() > 0 && !(shadow_blur == 0.0 && shadow_offset.is_zero()){
      true => {
        let mut paint = base_paint.clone();
        if let Some(filter) = image_filters::drop_shadow_only((0.0, 0.0), (sigma, sigma), shadow_color, None, None){
          paint.set_image_filter(filter); // this also knocks out any ctx.filter settings as a side-effect
        }
        Some(paint)
      }
      false => None
    }
  }

}

//
// Dye abstraction for Color / CanvasGradient / CanvasPattern
//

#[derive(Clone)]
pub enum Dye{
  Color(Color),
  Gradient(CanvasGradient),
  Pattern(CanvasPattern)
}

impl Dye{
  pub fn new<'a, T: This+Class>(cx: &mut CallContext<'a, T>, value: Handle<'a, JsValue>, style: PaintStyle) -> Result<Option<Self>, Throw> {
    let stash = if style == PaintStyle::Fill{ "fillShader" } else { "strokeShader" };
    match value{
      arg if arg.is_a::<JsCanvasGradient>() => {
        let gradient = cx.argument::<JsCanvasGradient>(0)?;
        stash_ref(cx, stash, arg)?;
        Ok(Some(cx.borrow(&gradient, |gradient| Dye::Gradient(gradient.clone()) )))
      },
      arg if arg.is_a::<JsCanvasPattern>() => {
        let pattern = cx.argument::<JsCanvasPattern>(0)?;
        stash_ref(cx, stash, arg)?;
        Ok(Some(cx.borrow(&pattern, |pattern| Dye::Pattern(pattern.clone()) )))
      },
      _ => {
        Ok(color_arg(cx, 0).map(Dye::Color))
      }
    }
  }

  pub fn value<'a, T: This+Class>(&self, cx: &mut CallContext<'a, T>, style: PaintStyle) -> JsResult<'a, JsValue> {
    let cache = if style == PaintStyle::Fill{ "fillShader" } else { "strokeShader" };
    match self{
      Dye::Gradient(..) => fetch_ref(cx, cache),
      Dye::Pattern(..)  => fetch_ref(cx, cache),
      Dye::Color(color) => color_to_css(cx, &color)
    }
  }

  pub fn mix_into(&self, paint: &mut Paint, alpha: f32){
    match self {
      Dye::Color(color) => {
        let mut color:Color4f = color.clone().into();
        color.a *= alpha;
        paint.set_color(color.to_color());
      },
      Dye::Gradient(gradient) =>{
        paint.set_shader(gradient.shader())
             .set_alpha_f(alpha);
      },
      Dye::Pattern(pattern) =>{
        paint.set_shader(pattern.shader())
             .set_alpha_f(alpha);
      }
    };
  }
}

// -- persistent references to js gradient/pattern objects ------------------------------

pub fn stash_ref<'a, T: This+Class>(cx: &mut CallContext<'a, T>, queue_name:&str, obj:Handle<'a, JsValue>) -> JsResult<'a, JsUndefined>{
  let this = cx.this().downcast::<JsContext2D>().or_throw(cx)?;
  let sym = symbol(cx, queue_name)?;
  let queue = match this.get(cx, sym)?.downcast::<JsArray>(){
    Ok(array) => array,
    Err(_e) => {
      // create ref queues lazily
      let array = JsArray::new(cx, 0);
      this.set(cx, sym, array)?;
      array
    }
  };

  let depth = cx.borrow(&this, |this| this.stack.len() as f64);
  let len = cx.number(depth + 1.0);
  let idx = cx.number(depth);
  let length = cx.string("length");

  queue.set(cx, length, len)?;
  queue.set(cx, idx, obj)?;
  Ok(cx.undefined())
}

pub fn fetch_ref<'a, T: This+Class>(cx: &mut CallContext<'a, T>, queue_name:&str) -> JsResult<'a, JsValue>{
  let this = cx.this().downcast::<JsContext2D>().or_throw(cx)?;
  let sym = symbol(cx, queue_name)?;
  let queue = this.get(cx, sym)?.downcast::<JsArray>().or_throw(cx)?;

  let length = cx.string("length");
  let len = queue.get(cx, length)?.downcast::<JsNumber>().or_throw(cx)?.value() as f64;
  let depth = cx.borrow(&this, |this| this.stack.len() as f64);
  let idx = cx.number(depth.min(len - 1.0));

  match queue.get(cx, idx){
    Ok(gradient) => Ok(gradient.upcast()),
    Err(_e) => Ok(cx.undefined().upcast())
  }
}

