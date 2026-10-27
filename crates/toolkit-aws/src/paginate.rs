//! Token pagination (`nextToken` / `NextToken`) with hard limits.
//!
//! The Python scripts walk `nextToken` loops by hand (and some return the token
//! to the caller). [`collect_pages`] is the shared Rust form: opaque tokens, an
//! empty token ends the walk (the Python `if token` rule), a repeated token is
//! an error (a stuck service would otherwise loop forever), and page and item
//! caps turn runaway responses into errors instead of memory growth or silent
//! truncation. Single-page callers simply call their fetch once and read
//! [`next_token_from`] themselves.

use std::collections::HashSet;

use toolkit_core::Json;

use crate::error::AwsError;

/// Default maximum pages per walk.
pub const DEFAULT_MAX_PAGES: usize = 1000;

/// Default maximum items per walk.
pub const DEFAULT_MAX_ITEMS: usize = 100_000;

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// Items on this page.
    pub items: Vec<T>,
    /// Token for the next page; `None` or empty ends the walk.
    pub next_token: Option<String>,
}

/// Caps on one pagination walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimits {
    /// Maximum pages fetched.
    pub max_pages: usize,
    /// Maximum items collected.
    pub max_items: usize,
}

impl Default for PageLimits {
    fn default() -> Self {
        Self {
            max_pages: DEFAULT_MAX_PAGES,
            max_items: DEFAULT_MAX_ITEMS,
        }
    }
}

/// Fetch every page and return all items in order.
///
/// `fetch` receives the token of the page to fetch (`None` for the first). Its
/// errors propagate unchanged.
///
/// # Errors
/// The first `fetch` error, or `Pagination` when a token repeats, more than
/// `limits.max_pages` pages would be needed, or more than `limits.max_items`
/// items would be collected. Tokens are never echoed in messages.
pub fn collect_pages<T>(
    limits: &PageLimits,
    mut fetch: impl FnMut(Option<&str>) -> Result<Page<T>, AwsError>,
) -> Result<Vec<T>, AwsError> {
    let mut items: Vec<T> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        if pages >= limits.max_pages {
            return Err(AwsError::pagination(format!(
                "more than {} pages; refusing to continue",
                limits.max_pages
            )));
        }
        let page = fetch(token.as_deref())?;
        pages += 1;
        if items.len().saturating_add(page.items.len()) > limits.max_items {
            return Err(AwsError::pagination(format!(
                "more than {} items; refusing to continue",
                limits.max_items
            )));
        }
        items.extend(page.items);
        match page.next_token.filter(|next| !next.is_empty()) {
            None => return Ok(items),
            Some(next) => {
                if !seen.insert(next.clone()) {
                    return Err(AwsError::pagination(
                        "the service returned a pagination token it had already returned",
                    ));
                }
                token = Some(next);
            }
        }
    }
}

/// Read the pagination token member `key` (for example `nextToken`) of a
/// response object.
///
/// Absent, `null` and empty-string tokens mean "no more pages".
///
/// # Errors
/// `Pagination` when the member exists but is neither a string nor `null`.
pub fn next_token_from(response: &Json, key: &str) -> Result<Option<String>, AwsError> {
    match response.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(token)) => Ok(Some(token.clone()).filter(|token| !token.is_empty())),
        Some(other) => Err(AwsError::pagination(format!(
            "{key} must be a string, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::parse_json;

    fn page(items: &[u32], next: Option<&str>) -> Page<u32> {
        Page {
            items: items.to_vec(),
            next_token: next.map(str::to_string),
        }
    }

    /// Serve `pages` in order, recording the token each request carried.
    fn serve(
        pages: Vec<Result<Page<u32>, AwsError>>,
        limits: &PageLimits,
    ) -> (Result<Vec<u32>, AwsError>, Vec<Option<String>>) {
        let mut remaining = pages.into_iter();
        let mut tokens = Vec::new();
        let result = collect_pages(limits, |token| {
            tokens.push(token.map(str::to_string));
            remaining.next().expect("fetched past the scripted pages")
        });
        (result, tokens)
    }

    #[test]
    fn single_page_without_token_is_returned_as_is() {
        let (result, tokens) = serve(vec![Ok(page(&[1, 2], None))], &PageLimits::default());
        assert_eq!(result.unwrap(), vec![1, 2]);
        assert_eq!(tokens, vec![None]);
    }

    #[test]
    fn pages_are_walked_in_order_passing_each_token() {
        let (result, tokens) = serve(
            vec![
                Ok(page(&[1, 2], Some("t1"))),
                Ok(page(&[], Some("t2"))),
                Ok(page(&[3], None)),
            ],
            &PageLimits::default(),
        );
        assert_eq!(result.unwrap(), vec![1, 2, 3]);
        assert_eq!(
            tokens,
            vec![None, Some("t1".to_string()), Some("t2".to_string())]
        );
    }

    #[test]
    fn an_empty_token_ends_the_walk() {
        let (result, tokens) = serve(vec![Ok(page(&[9], Some("")))], &PageLimits::default());
        assert_eq!(result.unwrap(), vec![9]);
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn a_repeated_token_is_an_error_that_does_not_echo_it() {
        let (result, _) = serve(
            vec![
                Ok(page(&[1], Some("secret-looking-token"))),
                Ok(page(&[2], Some("secret-looking-token"))),
            ],
            &PageLimits::default(),
        );
        let error = result.unwrap_err();
        assert_eq!(error.code(), "aws_pagination");
        assert!(error.to_string().contains("already returned"));
        assert!(!error.to_string().contains("secret-looking-token"));
    }

    #[test]
    fn the_page_cap_stops_a_long_walk_with_an_error() {
        let limits = PageLimits {
            max_pages: 3,
            max_items: 100,
        };
        let pages = (0..10)
            .map(|n| Ok(page(&[n], Some(format!("t{n}").as_str()))))
            .collect();
        let (result, tokens) = serve(pages, &limits);
        let error = result.unwrap_err();
        assert!(error.to_string().contains("more than 3 pages"));
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn a_walk_of_exactly_the_cap_succeeds() {
        let limits = PageLimits {
            max_pages: 2,
            max_items: 100,
        };
        let (result, _) = serve(
            vec![Ok(page(&[1], Some("t1"))), Ok(page(&[2], None))],
            &limits,
        );
        assert_eq!(result.unwrap(), vec![1, 2]);
    }

    #[test]
    fn the_item_cap_is_an_error_not_a_truncation() {
        let limits = PageLimits {
            max_pages: 10,
            max_items: 3,
        };
        let (result, _) = serve(
            vec![Ok(page(&[1, 2], Some("t1"))), Ok(page(&[3, 4], None))],
            &limits,
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("more than 3 items"));
        let (result, _) = serve(vec![Ok(page(&[1, 2, 3], None))], &limits);
        assert_eq!(result.unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn fetch_errors_propagate_and_stop_the_walk() {
        let (result, tokens) = serve(
            vec![
                Ok(page(&[1], Some("t1"))),
                Err(AwsError::NoCredentials),
                Ok(page(&[2], None)),
            ],
            &PageLimits::default(),
        );
        assert_eq!(result.unwrap_err(), AwsError::NoCredentials);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn zero_page_limit_fetches_nothing() {
        let limits = PageLimits {
            max_pages: 0,
            max_items: 10,
        };
        let (result, tokens) = serve(Vec::new(), &limits);
        assert!(result.is_err());
        assert!(tokens.is_empty());
    }

    #[test]
    fn next_token_is_read_from_response_objects() {
        let response = parse_json(r#"{"nextToken":"abc","items":[]}"#).unwrap();
        assert_eq!(
            next_token_from(&response, "nextToken").unwrap().as_deref(),
            Some("abc")
        );
        assert_eq!(next_token_from(&response, "NextToken").unwrap(), None);
        for absent in [
            r#"{"nextToken":null}"#,
            r#"{"nextToken":""}"#,
            "{}",
            "[]",
            "1",
        ] {
            let response = parse_json(absent).unwrap();
            assert_eq!(
                next_token_from(&response, "nextToken").unwrap(),
                None,
                "{absent}"
            );
        }
        let wrong = parse_json(r#"{"nextToken":5}"#).unwrap();
        let error = next_token_from(&wrong, "nextToken").unwrap_err();
        assert_eq!(
            error.to_string(),
            "pagination failed: nextToken must be a string, got integer"
        );
    }
}
