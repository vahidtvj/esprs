use crate::{Concat, Sms};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Key for grouping concatenated SMS parts: (sender, ref_num, total)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupKey {
    sender: String,
    ref_num: u16,
    total: u8,
}

/// A pending group of concatenated SMS parts
#[derive(Debug, Clone)]
struct PendingGroup {
    sender: String,
    parts: Vec<Part>,
    first_seen: Instant,
}

#[derive(Debug, Clone)]
struct Part {
    index: u16,
    seq: u8,
    message: String,
    date: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedMessage {
    pub sms: Sms,
    pub indices: Vec<u16>,
}

/// Configuration for the Reassembler
#[derive(Debug, Clone)]
pub struct ReassemblerConfig {
    /// Timeout for incomplete groups (default: 5 minutes)
    pub timeout: Duration,
}

impl Default for ReassemblerConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5 * 60),
        }
    }
}

/// Reassembles concatenated SMS messages from individual parts.
/// No AT port dependency - purely a data structure.
pub struct Reassembler {
    config: ReassemblerConfig,
    groups: HashMap<GroupKey, PendingGroup>,
}

impl Reassembler {
    /// Create a new Reassembler with default configuration (5 minute timeout)
    pub fn new() -> Self {
        Self::with_config(ReassemblerConfig::default())
    }

    /// Create a new Reassembler with custom configuration
    pub fn with_config(config: ReassemblerConfig) -> Self {
        Self {
            config,
            groups: HashMap::new(),
        }
    }

    /// Feed a part into the reassembler.
    /// Returns completed messages if any group is now complete.
    pub fn feed(
        &mut self,
        index: u16,
        sender: &str,
        date: &str,
        message: &str,
        concat: Option<&Concat>,
        now: Instant,
    ) -> Vec<CompletedMessage> {
        let mut completed = Vec::new();

        if let Some(concat_info) = concat {
            // This is a multipart message
            let key = GroupKey {
                sender: sender.to_string(),
                ref_num: concat_info.ref_num,
                total: concat_info.total,
            };

            // Check if this is a duplicate seq for this group
            let group = self
                .groups
                .entry(key.clone())
                .or_insert_with(|| PendingGroup {
                    sender: sender.to_string(),
                    parts: Vec::new(),
                    first_seen: now,
                });

            // Check for duplicate sequence number
            let is_duplicate = group.parts.iter().any(|p| p.seq == concat_info.seq);

            // For duplicate seq, store empty message (content ignored) but keep index
            let part_message = if is_duplicate {
                String::new()
            } else {
                message.to_string()
            };

            group.parts.push(Part {
                index,
                seq: concat_info.seq,
                message: part_message,
                date: date.to_string(),
            });

            // Check if group is complete (all unique seqs 1..total present)
            let unique_seqs: std::collections::HashSet<u8> =
                group.parts.iter().map(|p| p.seq).collect();
            if unique_seqs.len() == concat_info.total as usize
                && (1..=concat_info.total).all(|s| unique_seqs.contains(&s))
            {
                // Group is complete - build the completed message
                let mut sorted_parts = group.parts.clone();
                sorted_parts.sort_by_key(|p| p.seq);

                let mut full_message = String::new();
                for part in &sorted_parts {
                    full_message.push_str(&part.message);
                }

                // Date from seq 1 (even if it arrived last)
                let seq1_date = sorted_parts
                    .iter()
                    .find(|p| p.seq == 1)
                    .map(|p| p.date.clone())
                    .unwrap_or_else(|| date.to_string());

                let indices: Vec<u16> = group.parts.iter().map(|p| p.index).collect();

                completed.push(CompletedMessage {
                    sms: Sms {
                        sender: group.sender.clone(),
                        date: seq1_date,
                        message: full_message,
                    },
                    indices,
                });

                // Remove the completed group
                self.groups.remove(&key);
            }
        } else {
            // Non-multipart message - pass through immediately
            completed.push(CompletedMessage {
                sms: Sms {
                    sender: sender.to_string(),
                    date: date.to_string(),
                    message: message.to_string(),
                },
                indices: vec![index],
            });
        }

        completed
    }

    /// Expire old incomplete groups.
    /// Returns completed messages from expired groups (with available parts joined in seq order).
    pub fn expire(&mut self, now: Instant) -> Vec<CompletedMessage> {
        let mut completed = Vec::new();
        let timeout = self.config.timeout;

        // Collect keys of expired groups
        let expired_keys: Vec<GroupKey> = self
            .groups
            .iter()
            .filter(|(_, group)| now.duration_since(group.first_seen) > timeout)
            .map(|(key, _)| key.clone())
            .collect();

        for key in expired_keys {
            if let Some(group) = self.groups.remove(&key) {
                // Sort parts by seq
                let mut sorted_parts = group.parts;
                sorted_parts.sort_by_key(|p| p.seq);

                let mut full_message = String::new();
                for part in &sorted_parts {
                    full_message.push_str(&part.message);
                }

                // Date from seq 1 if available, else from first part
                let seq1_date = sorted_parts
                    .iter()
                    .find(|p| p.seq == 1)
                    .map(|p| p.date.clone())
                    .unwrap_or_else(|| {
                        sorted_parts
                            .first()
                            .map(|p| p.date.clone())
                            .unwrap_or_default()
                    });

                let indices: Vec<u16> = sorted_parts.iter().map(|p| p.index).collect();

                completed.push(CompletedMessage {
                    sms: Sms {
                        sender: group.sender,
                        date: seq1_date,
                        message: full_message,
                    },
                    indices,
                });
            }
        }

        completed
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_pdu_with_udh;
    use crate::test_vectors::concat_cases::*;

    #[test]
    fn test_gsm7_2a_both_orders() {
        // Parse the PDUs to get the actual message text
        let p1 = parse_pdu_with_udh(GSM7_2A_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(GSM7_2A_P2.0).unwrap();

        // Order 1: p1 then p2
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        completed = r.feed(
            2,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "AAAAAAAAAAHello World");
        assert_eq!(completed[0].indices, vec![1, 2]);
        assert_eq!(completed[0].sms.date, p1.sms.date); // date from seq 1

        // Order 2: p2 then p1
        let mut r = Reassembler::new();
        let now = Instant::now();
        completed = r.feed(
            2,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "AAAAAAAAAAHello World");
        assert_eq!(completed[0].indices, vec![2, 1]);
        assert_eq!(completed[0].sms.date, p1.sms.date); // date from seq 1
    }

    #[test]
    fn test_ucs2_07_pair() {
        let p1 = parse_pdu_with_udh(UCS2_07_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(UCS2_07_P2.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        completed = r.feed(
            2,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "سلام دنیا");
        assert_eq!(completed[0].indices, vec![1, 2]);
    }

    #[test]
    fn test_ucs2_1234_pair() {
        let p1 = parse_pdu_with_udh(UCS2_1234_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(UCS2_1234_P2.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        completed = r.feed(
            2,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "سلام دنیا");
        assert_eq!(completed[0].indices, vec![1, 2]);
    }

    #[test]
    fn test_gsm7_10_all_orders() {
        let p1 = parse_pdu_with_udh(GSM7_10_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(GSM7_10_P2.0).unwrap();
        let p3 = parse_pdu_with_udh(GSM7_10_P3.0).unwrap();

        let orders = vec![
            vec![(1, p1.clone()), (2, p2.clone()), (3, p3.clone())],
            vec![(1, p1.clone()), (3, p3.clone()), (2, p2.clone())],
            vec![(2, p2.clone()), (1, p1.clone()), (3, p3.clone())],
            vec![(2, p2.clone()), (3, p3.clone()), (1, p1.clone())],
            vec![(3, p3.clone()), (1, p1.clone()), (2, p2.clone())],
            vec![(3, p3.clone()), (2, p2.clone()), (1, p1.clone())],
        ];

        for order in orders {
            let mut r = Reassembler::new();
            let now = Instant::now();
            let last_idx = order.last().unwrap().0;
            for (idx, p) in order {
                let completed = r.feed(
                    idx,
                    &p.sms.sender,
                    &p.sms.date,
                    &p.sms.message,
                    p.concat.as_ref(),
                    now,
                );
                if idx == last_idx {
                    assert_eq!(completed.len(), 1, "Expected completion on last part");
                    assert_eq!(completed[0].sms.message, "one two three");
                    assert_eq!(completed[0].indices.len(), 3);
                    // All three indices present
                    let mut indices = completed[0].indices.clone();
                    indices.sort();
                    assert_eq!(indices, vec![1, 2, 3]);
                } else {
                    assert!(completed.is_empty());
                }
            }
        }
    }

    #[test]
    fn test_gsm7_10_with_duplicate_p2() {
        let p1 = parse_pdu_with_udh(GSM7_10_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(GSM7_10_P2.0).unwrap();
        let p3 = parse_pdu_with_udh(GSM7_10_P3.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        // First p2
        completed = r.feed(
            2,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        // Duplicate p2 (different index)
        completed = r.feed(
            5,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        // p3
        completed = r.feed(
            3,
            &p3.sms.sender,
            &p3.sms.date,
            &p3.sms.message,
            p3.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "one two three");
        // Indices should include the duplicate's index (5)
        let mut indices = completed[0].indices.clone();
        indices.sort();
        assert_eq!(indices, vec![1, 2, 3, 5]);
    }

    #[test]
    fn test_different_sender_no_merge() {
        let p1 = parse_pdu_with_udh(GSM7_2A_P1.0).unwrap();
        let p2 = parse_pdu_with_udh(GSM7_2A_P2.0).unwrap();
        let other = parse_pdu_with_udh(OTHER_2A_P1.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();

        // Feed gsm7_2a_p1 and other_2a_p1 together
        let mut completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        completed = r.feed(
            2,
            &other.sms.sender,
            &other.sms.date,
            &other.sms.message,
            other.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());
        // Then feed gsm7_2a_p2 - should complete gsm7_2a group
        completed = r.feed(
            3,
            &p2.sms.sender,
            &p2.sms.date,
            &p2.sms.message,
            p2.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "AAAAAAAAAAHello World");
        assert_eq!(completed[0].indices, vec![1, 3]);

        // Now expire - other_2a_p1 should yield alone
        let expired_now = now + Duration::from_secs(6 * 60);
        completed = r.expire(expired_now);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "X");
        assert_eq!(completed[0].indices, vec![2]);
    }

    #[test]
    fn test_expire_incomplete_group() {
        let p1 = parse_pdu_with_udh(GSM7_10_P1.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let completed = r.feed(
            1,
            &p1.sms.sender,
            &p1.sms.date,
            &p1.sms.message,
            p1.concat.as_ref(),
            now,
        );
        assert!(completed.is_empty());

        // Expire past timeout
        let expired_now = now + Duration::from_secs(6 * 60);
        let completed = r.expire(expired_now);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "one ");
        assert_eq!(completed[0].indices, vec![1]);
    }

    #[test]
    fn test_non_multipart_passes_through() {
        // Use a simple non-multipart PDU from ok_cases
        use crate::test_vectors::ok_cases::GSM7_HI;
        let pdu = parse_pdu_with_udh(GSM7_HI.0).unwrap();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let completed = r.feed(
            1,
            &pdu.sms.sender,
            &pdu.sms.date,
            &pdu.sms.message,
            pdu.concat.as_ref(),
            now,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].sms.message, "Hi");
        assert_eq!(completed[0].indices, vec![1]);
    }
}
