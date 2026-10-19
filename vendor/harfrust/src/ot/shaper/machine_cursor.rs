use core::ops::{Add, AddAssign, Sub, SubAssign};

// Similar to machine_index_t in harfbuzz, but Rust specific.
#[derive(Debug)]
pub struct MachineCursor<'a, T, F> {
    data: &'a [T],
    pred: F,
    pos: usize,
}

impl<'a, T, F> MachineCursor<'a, T, F>
where
    F: Fn(&[T], usize) -> bool,
{
    pub fn new(data: &'a [T], pred: F) -> Self {
        let mut pos = data.len();
        for candidate in 0..data.len() {
            if crate::cooperative::poll_loop().is_err() {
                break;
            }
            if pred(data, candidate) {
                pos = candidate;
                break;
            }
        }
        Self { data, pred, pos }
    }

    fn advance1(&mut self) {
        let start = self.pos.saturating_add(1).min(self.data.len());
        self.pos = self.data.len();
        for candidate in start..self.data.len() {
            shape_cooperative_abort!();
            if (self.pred)(self.data, candidate) {
                self.pos = candidate;
                break;
            }
        }
    }

    fn recede1(&mut self) {
        let start = self.pos;
        self.pos = 0;
        for candidate in (0..start).rev() {
            shape_cooperative_abort!();
            if (self.pred)(self.data, candidate) {
                self.pos = candidate;
                break;
            }
        }
    }

    pub fn index(&self) -> usize {
        self.pos
    }

    pub fn end(&self) -> Self
    where
        F: Clone,
    {
        Self {
            data: self.data,
            pred: self.pred.clone(),
            pos: self.data.len(),
        }
    }
}

impl<T, F> Add<usize> for MachineCursor<'_, T, F>
where
    F: Fn(&[T], usize) -> bool,
{
    type Output = Self;

    fn add(mut self, rhs: usize) -> Self::Output {
        for _ in 0..rhs {
            if crate::cooperative::poll_loop().is_err() {
                break;
            }
            self.advance1();
        }
        self
    }
}

impl<T, F> Sub<usize> for MachineCursor<'_, T, F>
where
    F: Fn(&[T], usize) -> bool,
{
    type Output = Self;

    fn sub(mut self, rhs: usize) -> Self::Output {
        for _ in 0..rhs {
            if crate::cooperative::poll_loop().is_err() {
                break;
            }
            self.recede1();
        }
        self
    }
}

impl<T, F> AddAssign<usize> for MachineCursor<'_, T, F>
where
    F: Fn(&[T], usize) -> bool,
{
    fn add_assign(&mut self, rhs: usize) {
        for _ in 0..rhs {
            shape_cooperative_abort!();
            self.advance1();
        }
    }
}

impl<T, F> SubAssign<usize> for MachineCursor<'_, T, F>
where
    F: Fn(&[T], usize) -> bool,
{
    fn sub_assign(&mut self, rhs: usize) {
        for _ in 0..rhs {
            shape_cooperative_abort!();
            self.recede1();
        }
    }
}

impl<T, F> PartialEq for MachineCursor<'_, T, F> {
    fn eq(&self, other: &Self) -> bool {
        self.pos == other.pos
    }
}

impl<T, F> Clone for MachineCursor<'_, T, F>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            data: self.data,
            pred: self.pred.clone(),
            pos: self.pos,
        }
    }
}

impl<T, F> Copy for MachineCursor<'_, T, F> where F: Copy {}
