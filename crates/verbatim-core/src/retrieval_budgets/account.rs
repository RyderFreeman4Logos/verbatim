//! Resource account: accumulated usage vs bound, typed partial/failure state.
//!
//! A [`ResourceAccount`] is the running ledger a request uses to decide whether
//! it may continue. It accumulates consumed pages, bytes, IOPS, await time, and
//! read amplification against a validated [`IoBudget`], and surfaces the typed
//! [`ResourceExhaustion`] the moment any dimension is exceeded — never an
//! unmarked empty or partial result.
//!
//! Contract only — no live counters, no `/proc` reader.

use super::io::{IoBudget, ResourceExhaustion};
use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// A running ledger of consumed I/O resources against a hard [`IoBudget`].
///
/// Every charge is checked immediately; the first charge that exceeds any
/// bound returns the typed exhaustion code and the account is marked exhausted.
/// Subsequent charges fail fast with the same code.
#[derive(Debug, Clone, Copy)]
pub struct ResourceAccount {
    budget: IoBudget,
    pages: u64,
    bytes: u64,
    iops: u64,
    await_micros: u64,
    read_amplification: u32,
    exhausted: Option<ResourceExhaustion>,
}

impl ResourceAccount {
    /// Constructs a fresh account bound to a validated I/O budget.
    pub const fn new(budget: IoBudget) -> Self {
        Self {
            budget,
            pages: 0,
            bytes: 0,
            iops: 0,
            await_micros: 0,
            read_amplification: 0,
            exhausted: None,
        }
    }

    /// Returns the budget this account is bound to.
    pub const fn budget(&self) -> IoBudget {
        self.budget
    }

    /// Returns the pages consumed so far.
    pub const fn pages(&self) -> u64 {
        self.pages
    }

    /// Returns the bytes consumed so far.
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns the IOPS consumed so far.
    pub const fn iops(&self) -> u64 {
        self.iops
    }

    /// Returns the await time consumed so far (microseconds).
    pub const fn await_micros(&self) -> u64 {
        self.await_micros
    }

    /// Returns the measured read-amplification ratio (numerator over
    /// [`READ_AMP_DENOMINATOR`](super::io::READ_AMP_DENOMINATOR)).
    pub const fn read_amplification(&self) -> u32 {
        self.read_amplification
    }

    /// Returns the typed exhaustion state if the account has been exhausted.
    pub const fn exhaustion(&self) -> Option<ResourceExhaustion> {
        self.exhausted
    }

    /// Attempts to charge `pages`, `bytes`, one IOP, `await_micros`, and the
    /// current read-amplification ratio. Returns `Err` with the typed code on
    /// the first dimension exceeded; further charges fail fast.
    pub fn charge_read(
        &mut self,
        pages: u64,
        bytes: u64,
        await_micros: u64,
        read_amplification: u32,
    ) -> RetrievalBudgetResult<()> {
        if let Some(code) = self.exhausted {
            return Err(RetrievalBudgetError::new(code.into()));
        }
        let (new_pages, new_bytes) =
            match (self.pages.checked_add(pages), self.bytes.checked_add(bytes)) {
                (Some(p), Some(b)) => (p, b),
                _ => {
                    self.exhausted = Some(ResourceExhaustion::PageBudgetExceeded);
                    return Err(RetrievalBudgetError::new(
                        RetrievalBudgetDiagnosticCode::PageBudgetExceeded,
                    ));
                }
            };
        let new_iops = match self.iops.checked_add(1) {
            Some(n) => n,
            None => {
                self.exhausted = Some(ResourceExhaustion::IopsExceeded);
                return Err(RetrievalBudgetError::new(
                    RetrievalBudgetDiagnosticCode::IopsExceeded,
                ));
            }
        };
        let new_await = match self.await_micros.checked_add(await_micros) {
            Some(n) => n,
            None => {
                self.exhausted = Some(ResourceExhaustion::AwaitExceeded);
                return Err(RetrievalBudgetError::new(
                    RetrievalBudgetDiagnosticCode::InvalidAwaitBudget,
                ));
            }
        };
        self.pages = new_pages;
        self.bytes = new_bytes;
        self.iops = new_iops;
        self.await_micros = new_await;
        self.read_amplification = read_amplification;
        match self.budget.exhaustion(
            self.pages,
            self.bytes,
            self.iops,
            self.await_micros,
            self.read_amplification,
        ) {
            Some(code) => {
                self.exhausted = Some(code);
                Err(RetrievalBudgetError::new(code.into()))
            }
            None => Ok(()),
        }
    }

    /// Returns `Ok(())` if every consumed dimension is still within bounds.
    pub fn check(&self) -> RetrievalBudgetResult<()> {
        match self.budget.exhaustion(
            self.pages,
            self.bytes,
            self.iops,
            self.await_micros,
            self.read_amplification,
        ) {
            Some(code) => Err(RetrievalBudgetError::new(code.into())),
            None => Ok(()),
        }
    }
}
