// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use db::auth::SessionHash;
use failure::{Error, format_err};
use serde::{Deserialize, Serialize};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use std::collections::BTreeMap;
use std::ops::Not;
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub struct TopLevel<'a> {
    pub time_zone_name: &'a str,

    // Use a custom serializer which presents the map's values as a sequence and includes the
    // "days" attribute or not, according to the bool in the tuple.
    #[serde(serialize_with = "TopLevel::serialize_cameras")]
    pub cameras: (&'a db::LockedDatabase, bool),

    pub session: Option<Session>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct Session {
    pub username: String,

    #[serde(serialize_with = "Session::serialize_csrf")]
    pub csrf: SessionHash,
}

impl Session {
    fn serialize_csrf<S>(csrf: &SessionHash, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let mut tmp = [0u8; 32];
        csrf.encode_base64(&mut tmp);
        serializer.serialize_str(::std::str::from_utf8(&tmp[..]).expect("base64 is UTF-8"))
    }
}

/// JSON serialization wrapper for a single camera when processing `/api/` and
/// `/api/cameras/<uuid>/`. See `design/api.md` for details.
#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct Camera<'a> {
    pub uuid: Uuid,
    pub short_name: &'a str,
    pub description: &'a str,

    #[serde(serialize_with = "Camera::serialize_streams")]
    pub streams: [Option<Stream<'a>>; 2],
}

#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct Stream<'a> {
    pub retain_bytes: i64,
    pub min_start_time_90k: Option<i64>,
    pub max_end_time_90k: Option<i64>,
    pub total_duration_90k: i64,
    pub total_sample_file_bytes: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "Stream::serialize_days")]
    pub days: Option<&'a BTreeMap<db::StreamDayKey, db::StreamDayValue>>,
}

impl<'a> Camera<'a> {
    pub fn wrap(c: &'a db::Camera, db: &'a db::LockedDatabase, include_days: bool) -> Result<Self, Error> {
        Ok(Camera {
            uuid: c.uuid,
            short_name: &c.short_name,
            description: &c.description,
            streams: [
                Stream::wrap(db, c.streams[0], include_days)?,
                Stream::wrap(db, c.streams[1], include_days)?,
            ],
        })
    }

    fn serialize_streams<S>(streams: &[Option<Stream<'a>>; 2], serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let mut map = serializer.serialize_map(Some(streams.len()))?;
        for (i, s) in streams.iter().enumerate() {
            if let &Some(ref s) = s {
                map.serialize_key(db::StreamType::from_index(i).expect("invalid stream type index").as_str())?;
                map.serialize_value(s)?;
            }
        }
        map.end()
    }
}

impl<'a> Stream<'a> {
    fn wrap(db: &'a db::LockedDatabase, id: Option<i32>, include_days: bool) -> Result<Option<Self>, Error> {
        let id = match id {
            Some(id) => id,
            None => return Ok(None),
        };
        let s = db.streams_by_id().get(&id).ok_or_else(|| format_err!("missing stream {}", id))?;
        Ok(Some(Stream {
            retain_bytes: s.retain_bytes,
            min_start_time_90k: s.range.as_ref().map(|r| r.start.0),
            max_end_time_90k: s.range.as_ref().map(|r| r.end.0),
            total_duration_90k: s.duration.0,
            total_sample_file_bytes: s.sample_file_bytes,
            days: if include_days { Some(&s.days) } else { None },
        }))
    }

    fn serialize_days<S>(days: &Option<&BTreeMap<db::StreamDayKey, db::StreamDayValue>>,
                         serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let days = match *days {
            Some(d) => d,
            None => return serializer.serialize_none(),
        };
        let mut map = serializer.serialize_map(Some(days.len()))?;
        for (k, v) in days {
            map.serialize_key(k.as_ref())?;
            let bounds = k.bounds();
            map.serialize_value(&StreamDayValue{
                start_time_90k: bounds.start.0,
                end_time_90k: bounds.end.0,
                total_duration_90k: v.duration.0,
            })?;
        }
        map.end()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
struct StreamDayValue {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub total_duration_90k: i64,
}

impl<'a> TopLevel<'a> {
    /// Serializes cameras as a list (rather than a map), optionally including the `days` field.
    fn serialize_cameras<S>(cameras: &(&db::LockedDatabase, bool),
                            serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let (db, include_days) = *cameras;
        let cs = db.cameras_by_id();
        let mut seq = serializer.serialize_seq(Some(cs.len()))?;
        for (_, c) in cs {
            seq.serialize_element(&Camera::wrap(c, db, include_days).unwrap())?;  // TODO: no unwrap.
        }
        seq.end()
    }
}

#[derive(Debug, Serialize)]
pub struct ListRecordings {
    pub recordings: Vec<Recording>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct Recording {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub sample_file_bytes: i64,
    pub video_samples: i64,
    pub video_sample_entry_sha1: String,
    pub start_id: i32,
    pub open_id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_uncommitted: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<i32>,
    pub video_sample_entry_width: u16,
    pub video_sample_entry_height: u16,

    #[serde(skip_serializing_if = "Not::not")]
    pub growing: bool,
}

#[derive(Debug, Deserialize)]
pub struct SampleFileDirPath {
    pub path: String
}
