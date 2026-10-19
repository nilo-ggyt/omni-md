//! 原trak选择/插值的有限记录游标；真实points与Scale沿同计划传入。
use super::*;

#[derive(Clone, Copy)]
enum Stage {
    Header,
    I,
    J,
    Value,
    Finish,
}
pub(super) struct Tracking {
    stage: Stage,
    i: usize,
    j: usize,
    entry: usize,
    row: usize,
    previous: Option<(f32, f32)>,
    a: f32,
    b: f32,
}
impl Tracking {
    pub(super) fn new() -> Self {
        Self {
            stage: Stage::Header,
            i: 0,
            j: 0,
            entry: 0,
            row: 0,
            previous: None,
            a: 0.0,
            b: 0.0,
        }
    }
    pub(super) fn step(
        &mut self,
        layout: crate::LayoutData<'_>,
        scale: crate::Scale,
        points: Option<f32>,
    ) -> Result<Option<i32>, Rejection> {
        let Some(trak) = layout.aat.trak.as_ref() else {
            return Ok(Some(0));
        };
        let Some(data) = trak
            .horiz()
            .transpose()
            .map_err(|_| Rejection::LookupUnavailable)?
        else {
            return Ok(Some(0));
        };
        let sizes = data
            .size_table(trak.offset_data())
            .ok_or(Rejection::LookupUnavailable)?;
        let tracks = data.track_table();
        if sizes.is_empty() || tracks.is_empty() {
            return Ok(Some(0));
        }
        let mut ptem = points.unwrap_or(0.0);
        if ptem <= 0.0 {
            ptem = 12.0
        }
        match self.stage {
            Stage::Header => {
                self.j = tracks.len() - 1;
                self.stage = if tracks.len() == 1 {
                    Stage::Value
                } else {
                    Stage::I
                };
            }
            Stage::I => {
                if self.i + 1 < tracks.len() && tracks[self.i + 1].track().to_f32() <= 0.0 {
                    self.i += 1;
                } else {
                    self.stage = Stage::J;
                }
            }
            Stage::J => {
                if self.j > 0 && tracks[self.j - 1].track().to_f32() >= 0.0 {
                    self.j -= 1;
                } else {
                    self.stage = Stage::Value;
                }
            }
            Stage::Value => {
                let index = if self.entry == 0 { self.i } else { self.j };
                let values = tracks[index]
                    .per_size_values(
                        trak.offset_data(),
                        u16::try_from(sizes.len()).map_err(|_| Rejection::MetadataBudget)?,
                    )
                    .ok_or(Rejection::LookupUnavailable)?;
                let count = sizes.len().min(values.len());
                let value = if self.row == count {
                    self.previous.map_or(0.0, |(_, value)| value)
                } else {
                    let size = sizes[self.row].get().to_f32();
                    let value = f32::from(values[self.row].get());
                    if size < ptem {
                        self.previous = Some((size, value));
                        self.row += 1;
                        return Ok(None);
                    }
                    match self.previous {
                        None => value,
                        Some((prior_size, prior_value)) => {
                            if (size - prior_size).abs() < f32::EPSILON {
                                f32::midpoint(prior_value, value)
                            } else {
                                prior_value
                                    + (ptem - prior_size) / (size - prior_size)
                                        * (value - prior_value)
                            }
                        }
                    }
                };
                if self.entry == 0 {
                    self.a = value;
                    if self.i == self.j {
                        self.b = value;
                        self.stage = Stage::Finish;
                    } else {
                        self.entry = 1;
                        self.row = 0;
                        self.previous = None;
                    }
                } else {
                    self.b = value;
                    self.stage = Stage::Finish;
                }
            }
            Stage::Finish => {
                let value = if self.i == self.j {
                    self.a
                } else {
                    let t0 = tracks[self.i].track().to_f32();
                    let t1 = tracks[self.j].track().to_f32();
                    let interp = if (t1 - t0).abs() < f32::EPSILON {
                        0.0
                    } else {
                        (0.0 - t0) / (t1 - t0)
                    };
                    self.a + interp * (self.b - self.a)
                };
                return Ok(Some(scale.scale_x_f(value)));
            }
        }
        Ok(None)
    }
}
