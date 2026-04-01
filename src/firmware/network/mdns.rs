// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

#![allow(
    clippy::large_stack_frames,
    reason = "mDNS task uses fixed packet buffers and macro-generated async wrappers"
)]

use embassy_net::{IpAddress, Stack};
use embassy_net::udp::UdpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;

use crate::firmware::{shared, status};

const MDNS_TTL_SECS: u32 = 120;
const MDNS_ANNOUNCE_INTERVAL_SECS: u64 = 30;

fn eq_ascii_ignore_case(a: u8, b: u8) -> bool {
    a.eq_ignore_ascii_case(&b)
}

#[derive(Clone, Copy)]
struct MdnsQuestionMatch {
    prefer_unicast_response: bool,
    qtype: u16,
    qclass_raw: u16,
}

fn bytes_eq_ascii_ignore_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if !eq_ascii_ignore_case(a[i], b[i]) {
            return false;
        }
    }
    true
}

fn dns_name_matches_host_local(
    packet: &[u8],
    start: usize,
    hostname: &[u8],
) -> Option<(bool, usize)> {
    let mut cursor = start;
    let mut jumped = false;
    let mut next = start;
    let mut depth = 0u8;
    let mut label_index = 0u8;
    let mut matched = true;

    while depth < 16 {
        if cursor >= packet.len() {
            return None;
        }

        let len = packet[cursor];
        if (len & 0xC0) == 0xC0 {
            if cursor + 1 >= packet.len() {
                return None;
            }
            let ptr = (((len & 0x3F) as usize) << 8) | packet[cursor + 1] as usize;
            if ptr >= packet.len() {
                return None;
            }
            if !jumped {
                next = cursor + 2;
                jumped = true;
            }
            cursor = ptr;
            depth = depth.saturating_add(1);
            continue;
        }

        if (len & 0xC0) != 0 {
            return None;
        }

        cursor += 1;
        if len == 0 {
            if !jumped {
                next = cursor;
            }
            return Some((matched && label_index == 2, next));
        }

        let label_len = len as usize;
        if cursor + label_len > packet.len() {
            return None;
        }
        let label = &packet[cursor..cursor + label_len];

        if matched {
            let this_matches = match label_index {
                0 => bytes_eq_ascii_ignore_case(label, hostname),
                1 => bytes_eq_ascii_ignore_case(label, b"local"),
                _ => false,
            };
            if !this_matches {
                matched = false;
            }
        }

        label_index = label_index.saturating_add(1);
        cursor += label_len;
    }

    None
}

fn question_matches_hostname(query: &[u8], hostname: &[u8]) -> Option<MdnsQuestionMatch> {
    if query.len() < 12 {
        return None;
    }

    let qdcount = u16::from_be_bytes([query[4], query[5]]) as usize;
    let mut offset = 12usize;

    for _ in 0..qdcount {
        let (matches_name, next) = dns_name_matches_host_local(query, offset, hostname)?;
        if next + 4 > query.len() {
            return None;
        }

        let qtype = u16::from_be_bytes([query[next], query[next + 1]]);
        let qclass_raw = u16::from_be_bytes([query[next + 2], query[next + 3]]);
        let qclass = qclass_raw & 0x7fff;

        if matches_name && qclass == 1 && (qtype == 1 || qtype == 28 || qtype == 255) {
            return Some(MdnsQuestionMatch {
                prefer_unicast_response: (qclass_raw & 0x8000) != 0,
                qtype,
                qclass_raw,
            });
        }

        offset = next + 4;
    }

    None
}

#[allow(
    clippy::large_stack_frames,
    reason = "mDNS response building uses fixed-size packet buffers for deterministic no_std behavior"
)]
fn build_mdns_response(
    query: &[u8],
    hostname: &[u8],
    ipv4: Option<[u8; 4]>,
    ipv6: Option<[u8; 16]>,
    out: &mut [u8],
) -> Option<(usize, MdnsQuestionMatch)> {
    let question = question_matches_hostname(query, hostname)?;

    let (answer_type, answer_rdlen): (u16, u16) = match question.qtype {
        1 => {
            ipv4?;
            (1, 4)
        }
        28 => {
            ipv6?;
            (28, 16)
        }
        // ANY: prefer IPv4 when available, otherwise advertise IPv6.
        255 => {
            if ipv4.is_some() {
                (1, 4)
            } else {
                ipv6?;
                (28, 16)
            }
        }
        _ => return None,
    };

    let question_len = hostname.len() + 12;
    let total_len = 12 + question_len + 12 + answer_rdlen as usize;
    if out.len() < total_len {
        return None;
    }

    out[0] = query[0];
    out[1] = query[1];
    out[2] = 0x84;
    out[3] = 0x00;
    out[4] = 0x00;
    out[5] = 0x01;
    out[6] = 0x00;
    out[7] = 0x01;
    out[8] = 0x00;
    out[9] = 0x00;
    out[10] = 0x00;
    out[11] = 0x00;

    let mut q = 12usize;
    out[q] = hostname.len() as u8;
    q += 1;
    out[q..q + hostname.len()].copy_from_slice(hostname);
    q += hostname.len();
    out[q] = 5;
    q += 1;
    out[q..q + 5].copy_from_slice(b"local");
    q += 5;
    out[q] = 0;
    q += 1;
    out[q..q + 2].copy_from_slice(&question.qtype.to_be_bytes());
    q += 2;
    out[q..q + 2].copy_from_slice(&question.qclass_raw.to_be_bytes());
    q += 2;

    let mut i = q;
    out[i] = 0xC0;
    out[i + 1] = 0x0C;
    out[i + 2..i + 4].copy_from_slice(&answer_type.to_be_bytes());
    out[i + 4] = 0x80;
    out[i + 5] = 0x01;
    out[i + 6] = 0x00;
    out[i + 7] = 0x00;
    out[i + 8] = 0x00;
    out[i + 9] = 0x78;
    out[i + 10..i + 12].copy_from_slice(&answer_rdlen.to_be_bytes());
    let rdata_start = i + 12;
    let rdata_end = rdata_start + answer_rdlen as usize;
    if answer_type == 1 {
        out[rdata_start..rdata_end].copy_from_slice(&ipv4?);
    } else {
        out[rdata_start..rdata_end].copy_from_slice(&ipv6?);
    }
    i = rdata_end;

    Some((i, question))
}

fn append_bytes(out: &mut [u8], index: &mut usize, bytes: &[u8]) -> Option<()> {
    if *index + bytes.len() > out.len() {
        return None;
    }
    out[*index..*index + bytes.len()].copy_from_slice(bytes);
    *index += bytes.len();
    Some(())
}

fn append_u16(out: &mut [u8], index: &mut usize, value: u16) -> Option<()> {
    append_bytes(out, index, &value.to_be_bytes())
}

fn append_u32(out: &mut [u8], index: &mut usize, value: u32) -> Option<()> {
    append_bytes(out, index, &value.to_be_bytes())
}

fn append_dns_name(out: &mut [u8], index: &mut usize, labels: &[&[u8]]) -> Option<()> {
    for label in labels {
        if label.len() > 63 {
            return None;
        }
        append_bytes(out, index, &[label.len() as u8])?;
        append_bytes(out, index, label)?;
    }
    append_bytes(out, index, &[0])
}

fn build_mdns_announcement(
    hostname: &[u8],
    ipv4: Option<[u8; 4]>,
    ipv6: Option<[u8; 16]>,
    out: &mut [u8],
) -> Option<usize> {
    if ipv4.is_none() && ipv6.is_none() {
        return None;
    }

    let mut answer_count: u16 = 4;
    if ipv4.is_some() {
        answer_count += 1;
    }
    if ipv6.is_some() {
        answer_count += 1;
    }

    let mut i = 0usize;

    // Unsolicited mDNS answer packet with service and host records.
    append_u16(out, &mut i, 0x0000)?; // transaction id
    append_u16(out, &mut i, 0x8400)?; // response + authoritative
    append_u16(out, &mut i, 0x0000)?; // qdcount
    append_u16(out, &mut i, answer_count)?; // ancount
    append_u16(out, &mut i, 0x0000)?; // nscount
    append_u16(out, &mut i, 0x0000)?; // arcount

    let services_labels: [&[u8]; 4] = [b"_services", b"_dns-sd", b"_udp", b"local"];
    let http_service_labels: [&[u8]; 3] = [b"_http", b"_tcp", b"local"];
    let http_instance_labels: [&[u8]; 4] = [hostname, b"_http", b"_tcp", b"local"];
    let host_labels: [&[u8]; 2] = [hostname, b"local"];

    // PTR _services._dns-sd._udp.local -> _http._tcp.local
    append_dns_name(out, &mut i, &services_labels)?;
    append_u16(out, &mut i, 12)?; // PTR
    append_u16(out, &mut i, 0x0001)?; // IN
    append_u32(out, &mut i, MDNS_TTL_SECS)?;
    let rdlen_pos = i;
    append_u16(out, &mut i, 0)?;
    let rdata_start = i;
    append_dns_name(out, &mut i, &http_service_labels)?;
    let rdata_len = (i - rdata_start) as u16;
    out[rdlen_pos..rdlen_pos + 2].copy_from_slice(&rdata_len.to_be_bytes());

    // PTR _http._tcp.local -> <hostname>._http._tcp.local
    append_dns_name(out, &mut i, &http_service_labels)?;
    append_u16(out, &mut i, 12)?; // PTR
    append_u16(out, &mut i, 0x0001)?; // IN
    append_u32(out, &mut i, MDNS_TTL_SECS)?;
    let rdlen_pos = i;
    append_u16(out, &mut i, 0)?;
    let rdata_start = i;
    append_dns_name(out, &mut i, &http_instance_labels)?;
    let rdata_len = (i - rdata_start) as u16;
    out[rdlen_pos..rdlen_pos + 2].copy_from_slice(&rdata_len.to_be_bytes());

    // SRV <hostname>._http._tcp.local -> <hostname>.local:80
    append_dns_name(out, &mut i, &http_instance_labels)?;
    append_u16(out, &mut i, 33)?; // SRV
    append_u16(out, &mut i, 0x8001)?; // IN, cache-flush
    append_u32(out, &mut i, MDNS_TTL_SECS)?;
    let rdlen_pos = i;
    append_u16(out, &mut i, 0)?;
    let rdata_start = i;
    append_u16(out, &mut i, 0)?; // priority
    append_u16(out, &mut i, 0)?; // weight
    append_u16(out, &mut i, super::HTTP_PORT)?; // port
    append_dns_name(out, &mut i, &host_labels)?; // target host
    let rdata_len = (i - rdata_start) as u16;
    out[rdlen_pos..rdlen_pos + 2].copy_from_slice(&rdata_len.to_be_bytes());

    // TXT <hostname>._http._tcp.local (empty TXT payload)
    append_dns_name(out, &mut i, &http_instance_labels)?;
    append_u16(out, &mut i, 16)?; // TXT
    append_u16(out, &mut i, 0x8001)?; // IN, cache-flush
    append_u32(out, &mut i, MDNS_TTL_SECS)?;
    append_u16(out, &mut i, 1)?;
    append_bytes(out, &mut i, &[0x00])?;

    if let Some(ipv4) = ipv4 {
        // A <hostname>.local -> IPv4
        append_dns_name(out, &mut i, &host_labels)?;
        append_u16(out, &mut i, 1)?; // A
        append_u16(out, &mut i, 0x8001)?; // IN, cache-flush
        append_u32(out, &mut i, MDNS_TTL_SECS)?;
        append_u16(out, &mut i, 4)?;
        append_bytes(out, &mut i, &ipv4)?;
    }

    if let Some(ipv6) = ipv6 {
        // AAAA <hostname>.local -> IPv6
        append_dns_name(out, &mut i, &host_labels)?;
        append_u16(out, &mut i, 28)?; // AAAA
        append_u16(out, &mut i, 0x8001)?; // IN, cache-flush
        append_u32(out, &mut i, MDNS_TTL_SECS)?;
        append_u16(out, &mut i, 16)?;
        append_bytes(out, &mut i, &ipv6)?;
    }

    Some(i)
}

#[allow(
    clippy::large_stack_frames,
    reason = "mDNS task keeps RX/TX packet buffers on stack during async loop"
)]
#[embassy_executor::task]
pub(super) async fn mdns_task(stack: Stack<'static>) {
    let rx_meta = super::MDNS_RX_META.take();
    let tx_meta = super::MDNS_TX_META.take();
    let rx_buffer = super::MDNS_RX_BUFFER.take();
    let tx_buffer = super::MDNS_TX_BUFFER.take();
    let recv_buf = super::MDNS_RECV_PACKET.take();
    let send_buf = super::MDNS_SEND_PACKET.take();

    // Reuse the same hostname normalization policy used by DHCP hostnames.
    let normalized_hostname = shared::normalized_dhcp_hostname(crate::device_hostname());
    let hostname = normalized_hostname.as_bytes();

    loop {
        stack.wait_config_up().await;

        if !stack.has_multicast_group(super::MDNS_MULTICAST) {
            match stack.join_multicast_group(super::MDNS_MULTICAST) {
                Ok(()) => {}
                Err(error) => {
                    println!("mdns: failed to join multicast group: {:?}", error);
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
            }
        }

        if !stack.has_multicast_group(super::MDNS_MULTICAST_V6)
            && let Err(error) = stack.join_multicast_group(super::MDNS_MULTICAST_V6)
        {
            println!("mdns: failed to join ipv6 multicast group: {:?}", error);
        }

        let mut socket = UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer);
        if let Err(error) = socket.bind(super::MDNS_PORT) {
            println!("mdns: bind failed: {:?}", error);
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }

        let mut announce_deadline = Instant::now();

        loop {
            if !stack.is_config_up() {
                break;
            }

            if Instant::now() >= announce_deadline {
                let ipv4 = status::ip_octets();
                let ipv6 = current_ipv6_octets(stack);
                if let Some(n) = build_mdns_announcement(hostname, ipv4, ipv6, send_buf)
                    && let Err(error) = socket
                        .send_to(&send_buf[..n], (super::MDNS_MULTICAST, super::MDNS_PORT))
                        .await
                {
                    println!("mdns: announce send failed: {:?}", error);
                }
                if let Some(n) = build_mdns_announcement(hostname, ipv4, ipv6, send_buf)
                    && let Err(error) = socket
                        .send_to(&send_buf[..n], (super::MDNS_MULTICAST_V6, super::MDNS_PORT))
                        .await
                {
                    println!("mdns: ipv6 announce send failed: {:?}", error);
                }
                announce_deadline =
                    Instant::now() + Duration::from_secs(MDNS_ANNOUNCE_INTERVAL_SECS);
            }

            let (packet, meta) =
                match with_timeout(Duration::from_secs(2), socket.recv_from(recv_buf)).await {
                    Ok(Ok((n, meta))) => (&recv_buf[..n], meta),
                    Ok(Err(_)) => continue,
                    Err(_) => continue,
                };

            let ipv4 = status::ip_octets();
            let ipv6 = current_ipv6_octets(stack);

            let Some((n, question)) = build_mdns_response(packet, hostname, ipv4, ipv6, send_buf)
            else {
                continue;
            };

            let prefer_unicast_response = question.prefer_unicast_response;

            if prefer_unicast_response {
                if let Err(error) = socket.send_to(&send_buf[..n], meta).await {
                    println!("mdns: unicast send failed to {:?}: {:?}", meta, error);
                }
            } else {
                let multicast_destination: (IpAddress, u16) = match meta.endpoint.addr {
                    IpAddress::Ipv6(_) => {
                        (IpAddress::Ipv6(super::MDNS_MULTICAST_V6), super::MDNS_PORT)
                    }
                    _ => (IpAddress::Ipv4(super::MDNS_MULTICAST), super::MDNS_PORT),
                };
                if let Err(error) = socket
                    .send_to(&send_buf[..n], multicast_destination)
                    .await
                {
                    println!("mdns: multicast send failed: {:?}", error);
                }
                if let Err(error) = socket.send_to(&send_buf[..n], meta).await {
                    println!(
                        "mdns: fallback unicast send failed to {:?}: {:?}",
                        meta, error
                    );
                }
            }
        }
    }
}

fn current_ipv6_octets(stack: Stack<'static>) -> Option<[u8; 16]> {
    stack.config_v6().map(|cfg| cfg.address.address().octets())
}
