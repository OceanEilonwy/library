//! Lending: anyone can lend an item out by giving a name; the date is
//! recorded automatically. An item has at most one active loan.

use anyhow::Result;
use rusqlite::{Connection, ErrorCode, params};

use super::now;
use crate::util;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loan {
    pub id: i64,
    pub item_id: i64,
    pub borrower: String,
    pub loaned_at: i64,
    pub returned_at: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoanError {
    EmptyName,
    AlreadyOut(String),
    NoSuchItem,
}

impl std::fmt::Display for LoanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoanError::EmptyName => write!(f, "Please give the borrower's name."),
            LoanError::AlreadyOut(who) => write!(f, "It's already out with {who}."),
            LoanError::NoSuchItem => write!(f, "That item no longer exists."),
        }
    }
}

impl std::error::Error for LoanError {}

pub fn lend(conn: &Connection, item_id: i64, borrower: &str) -> Result<Loan> {
    let borrower = util::clean(borrower, 80);
    if borrower.is_empty() {
        return Err(LoanError::EmptyName.into());
    }
    let exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM items WHERE id = ?1",
        [item_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(LoanError::NoSuchItem.into());
    }
    let ts = now();
    match conn.execute(
        "INSERT INTO loans (item_id, borrower, loaned_at) VALUES (?1, ?2, ?3)",
        params![item_id, borrower, ts],
    ) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
            let who = active(conn, item_id)?
                .map(|l| l.borrower)
                .unwrap_or_default();
            return Err(LoanError::AlreadyOut(who).into());
        }
        Err(e) => return Err(e.into()),
    }
    Ok(Loan {
        id: conn.last_insert_rowid(),
        item_id,
        borrower,
        loaned_at: ts,
        returned_at: None,
    })
}

/// Marks the active loan returned. Returning something that is not out is
/// harmless (two people pressing the button at once).
pub fn mark_returned(conn: &Connection, item_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE loans SET returned_at = ?2 WHERE item_id = ?1 AND returned_at IS NULL",
        params![item_id, now()],
    )?;
    Ok(n > 0)
}

pub fn active(conn: &Connection, item_id: i64) -> Result<Option<Loan>> {
    Ok(history(conn, item_id)?
        .into_iter()
        .find(|l| l.returned_at.is_none()))
}

pub fn history(conn: &Connection, item_id: i64) -> Result<Vec<Loan>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, item_id, borrower, loaned_at, returned_at FROM loans
         WHERE item_id = ?1 ORDER BY loaned_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok(Loan {
            id: r.get(0)?,
            item_id: r.get(1)?,
            borrower: r.get(2)?,
            loaned_at: r.get(3)?,
            returned_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::items::{self, tests::input};
    use crate::db::testing::temp_db;

    #[test]
    fn lend_return_and_history() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let id = items::create(c, &input(c, "Low", "David Bowie"))?;
            let loan = lend(c, id, "  Kasia ")?;
            assert_eq!(loan.borrower, "Kasia");
            let err = lend(c, id, "Tom").unwrap_err();
            assert_eq!(
                err.downcast_ref::<LoanError>(),
                Some(&LoanError::AlreadyOut("Kasia".into()))
            );
            assert!(mark_returned(c, id)?);
            assert!(!mark_returned(c, id)?, "second return is a no-op");
            lend(c, id, "Tom")?;
            let h = history(c, id)?;
            assert_eq!(h.len(), 2);
            assert_eq!(active(c, id)?.unwrap().borrower, "Tom");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn rejects_empty_names_and_missing_items() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let id = items::create(c, &input(c, "Low", ""))?;
            assert_eq!(
                lend(c, id, "   ").unwrap_err().downcast_ref::<LoanError>(),
                Some(&LoanError::EmptyName)
            );
            assert_eq!(
                lend(c, 9999, "Sam")
                    .unwrap_err()
                    .downcast_ref::<LoanError>(),
                Some(&LoanError::NoSuchItem)
            );
            Ok(())
        })
        .unwrap();
    }
}
