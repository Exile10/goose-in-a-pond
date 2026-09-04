//! Reading a WebDAV `multistatus` without pretending to implement WebDAV.
//!
//! Servers disagree about namespace prefixes (`d:href`, `D:href`, `href`), so everything here
//! matches on the LOCAL name and ignores the prefix; a different server must not break it.

use quick_xml::events::Event;
use quick_xml::Reader;

/// One `<response>` element, reduced to the parts a connector needs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DavResponse {
    pub href: String,
    /// `<resourcetype>` children by local name, e.g. `collection`, `calendar`.
    pub resource_types: Vec<String>,
    /// Text of `<displayname>`, when the server sent one.
    pub display_name: Option<String>,
    /// Text of `<calendar-data>`, i.e. the iCalendar document itself.
    pub calendar_data: Option<String>,
    /// `<getctag>` or `<sync-token>`: what makes the next sync incremental.
    pub ctag: Option<String>,
    /// An href nested inside a property such as `<current-user-principal>` or
    /// `<calendar-home-set>`, which is how discovery walks from one URL to the
    /// next.
    pub nested_href: Option<String>,
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or("").to_ascii_lowercase()
}

/// Parse a `multistatus` document into its responses.
///
/// Unknown elements are skipped rather than refused: a server that sends extra properties
/// is behaving correctly, and failing on them would tie the connector to the servers tested.
pub fn parse_multistatus(xml: &str) -> anyhow::Result<Vec<DavResponse>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut responses = Vec::new();
    let mut current: Option<DavResponse> = None;
    // The element stack, by local name, so text can be attributed to the
    // property that contains it rather than to whatever opened last.
    let mut stack: Vec<String> = Vec::new();
    // `<href>` appears both as the response's own subject and nested inside
    // properties; depth tells them apart.
    let mut seen_response_href = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local(e.name().as_ref());
                if name == "response" {
                    current = Some(DavResponse::default());
                    seen_response_href = false;
                }
                if name == "resourcetype" {
                    // Children of resourcetype are empty elements; collect them
                    // as they open.
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = local(e.name().as_ref());
                if stack.last().map(String::as_str) == Some("resourcetype") {
                    if let Some(cur) = current.as_mut() {
                        cur.resource_types.push(name);
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let Some(cur) = current.as_mut() else {
                    continue;
                };
                match stack.last().map(String::as_str) {
                    Some("href") => {
                        // The first href under a response is its own subject;
                        // any later one is nested in a property and is where
                        // discovery goes next.
                        if !seen_response_href
                            && !stack.iter().rev().skip(1).any(|s| {
                                s == "current-user-principal"
                                    || s == "calendar-home-set"
                                    || s == "owner"
                            })
                        {
                            cur.href = text;
                            seen_response_href = true;
                        } else {
                            cur.nested_href.get_or_insert(text);
                        }
                    }
                    Some("displayname") => cur.display_name = Some(text),
                    Some("calendar-data") => cur.calendar_data = Some(text),
                    Some("getctag") | Some("sync-token") => cur.ctag = Some(text),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = local(e.name().as_ref());
                stack.pop();
                if name == "response" {
                    if let Some(cur) = current.take() {
                        responses.push(cur);
                    }
                }
            }
            Ok(Event::Eof) => {
                // quick-xml reports Eof on a TRUNCATED document rather than an error, so an
                // interrupted reply would parse as a short list that the caller cannot tell
                // from an empty calendar. Refuse it instead.
                if !stack.is_empty() || current.is_some() {
                    return Err(anyhow::anyhow!(
                        "the calendar server's reply ended early, with {} element(s) unclosed",
                        stack.len()
                    ));
                }
                break;
            }
            Err(e) => return Err(anyhow::anyhow!("malformed CalDAV response: {e}")),
            _ => {}
        }
    }
    Ok(responses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_calendar_collection_is_recognised_whatever_prefix_the_server_uses() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/calendars/jerry/home/</D:href>
    <D:propstat><D:prop>
      <D:displayname>Home</D:displayname>
      <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
      <CS:getctag xmlns:CS="http://calendarserver.org/ns/">tag-1</CS:getctag>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].href, "/calendars/jerry/home/");
        assert_eq!(r[0].display_name.as_deref(), Some("Home"));
        assert!(r[0].resource_types.iter().any(|t| t == "calendar"));
        assert_eq!(r[0].ctag.as_deref(), Some("tag-1"));
    }

    /// Prefix-blindness is the property that makes this work against more than
    /// one server. iCloud, Google and Nextcloud all choose differently.
    #[test]
    fn an_unprefixed_document_parses_identically() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/c/</href>
    <propstat><prop><resourcetype><collection/><calendar/></resourcetype></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].href, "/c/");
        assert!(r[0].resource_types.iter().any(|t| t == "calendar"));
    }

    /// Discovery depends on this distinction. Confusing the two hrefs sends the
    /// next request back to the URL it just came from, which loops.
    #[test]
    fn an_href_inside_a_property_is_kept_apart_from_the_responses_own() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/</href>
    <propstat><prop>
      <current-user-principal><href>/principals/jerry/</href></current-user-principal>
    </prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].href, "/");
        assert_eq!(r[0].nested_href.as_deref(), Some("/principals/jerry/"));
    }

    #[test]
    fn calendar_data_comes_back_whole() {
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response><href>/c/e1.ics</href>
    <propstat><prop><C:calendar-data>BEGIN:VCALENDAR
END:VCALENDAR</C:calendar-data></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert!(r[0].calendar_data.as_deref().unwrap().contains("VCALENDAR"));
    }

    /// A server sending properties this connector has never heard of is
    /// behaving correctly; refusing them would restrict it to the servers it
    /// happened to be written against.
    #[test]
    fn unknown_properties_are_skipped_rather_than_refused() {
        let xml = r#"<multistatus xmlns="DAV:">
  <response><href>/c/</href>
    <propstat><prop><some-vendor-extension>x</some-vendor-extension>
      <displayname>Work</displayname></prop></propstat>
  </response></multistatus>"#;
        let r = parse_multistatus(xml).unwrap();
        assert_eq!(r[0].display_name.as_deref(), Some("Work"));
    }

    /// A truncated reply must not read as an empty calendar. This is the
    /// difference between "nothing is scheduled" and "the connection dropped",
    /// and only one of them is worth telling a household.
    #[test]
    fn a_truncated_reply_is_an_error_not_an_empty_list() {
        let err = parse_multistatus("<multistatus><response><href>/c/</href>").unwrap_err();
        assert!(err.to_string().contains("ended early"), "{err}");
        // And the well-formed empty case still parses, or the guard above is
        // just refusing everything.
        assert!(
            parse_multistatus("<multistatus xmlns=\"DAV:\"></multistatus>")
                .unwrap()
                .is_empty()
        );
    }
}
