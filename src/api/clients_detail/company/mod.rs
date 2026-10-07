//! `GET /clients/{id}` -- the company page's data: company fields, its facilities, owners and whether Elavon is active.

mod dto;
mod handler;
mod queries;

pub use handler::get_company_detail;
