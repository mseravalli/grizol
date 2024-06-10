use crate::syncthing::{BlockInfo, ClusterConfig, Counter, FileInfo, Index, Request};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long a requests stays in the queue before being removed and being resent.
// TODO: put this in the config
const MAX_TIME_IN_QUEUE: Duration = Duration::from_secs(60 * 60 * 12);

#[derive(Debug, Hash, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct OutgoingRequest {
    folder: String,
    name: String,
    offset: i64,
    size: i32,
    hash: [u8; 32],
    from_temporary: bool,
}

impl From<Request> for OutgoingRequest {
    fn from(other: Request) -> Self {
        OutgoingRequest {
            folder: other.folder,
            name: other.name,
            offset: other.offset,
            size: other.size,
            hash: other.hash.try_into().unwrap(),
            from_temporary: other.from_temporary,
        }
    }
}

impl From<&Request> for OutgoingRequest {
    fn from(other: &Request) -> Self {
        OutgoingRequest {
            folder: other.folder.clone(),
            name: other.name.clone(),
            offset: other.offset,
            size: other.size,
            hash: other.hash.clone().try_into().unwrap(),
            from_temporary: other.from_temporary,
        }
    }
}

// Stores the same information, but in different data structures to be able to access them more
// easily.
#[derive(Debug, Default)]
pub struct OutgoingRequests {
    requests_by_id: HashMap<i32, Request>,
    requests_by_request: BTreeMap<OutgoingRequest, (i32, Instant)>,
    pending_requests: HashSet<OutgoingRequest>,
    requested_size: usize,
}

impl OutgoingRequests {
    pub fn get<'a>(&'a self, request_id: &i32) -> Option<&'a Request> {
        self.requests_by_id.get(request_id)
    }
    pub fn remove(&mut self, request_id: &i32) -> Option<Request> {
        let request = self.requests_by_id.remove(request_id);
        if let Some(ref r) = request {
            let outgoing_request = r.into();
            let request_id = self.requests_by_request.remove(&outgoing_request);
            // if we remove something from requests_by_id, the same element must be in
            // requests_by_type as well. If that's not the case, the invariant of OutgoingRequests
            // does not hold.
            assert!(request_id.is_some());

            let pending_removed = self.pending_requests.remove(&outgoing_request);
            assert!(pending_removed);
            self.requested_size -= outgoing_request.size as usize;
            assert!(self.requested_size >= 0);
        };
        request
    }

    /// Returns whether it was inserted or not
    pub fn insert(&mut self, request_id: i32, request: Request) -> Option<Request> {
        let outgoing_request: OutgoingRequest = request.clone().into();

        if self.requests_by_request.contains_key(&outgoing_request) {
            return None;
        }

        self.requests_by_request
            .insert(outgoing_request, (request_id, Instant::now()));

        self.requests_by_id.insert(request_id, request.clone());

        Some(request)
    }

    /// Removes requests that have been in the queue for more than [MAX_TIME_IN_QUEUE].
    // This runs in O(n). It should be ok given that it should not be called very often, to improve
    // the situation, it might be possible to add a new data structure in [OutgoingRequests] to
    // store creation_time.
    pub fn remove_old_requests(&mut self) {
        let now = Instant::now();
        let in_queue_less_than_max =
            |creation_time: &Instant| now - *creation_time < MAX_TIME_IN_QUEUE;
        let waiting_requests_ids: HashSet<i32> = self
            .requests_by_request
            .iter()
            .filter(|(_, (_, creation_time))| in_queue_less_than_max(creation_time))
            .map(|(_, (request_id, _))| *request_id)
            .collect();
        self.requests_by_id
            .retain(|request_id, _| waiting_requests_ids.contains(request_id));
        self.requests_by_request
            .retain(|_, (request_id, _)| waiting_requests_ids.contains(request_id));

        let size_to_remove: usize = self
            .pending_requests
            .iter()
            .filter(|request| !self.requests_by_request.contains_key(*request))
            .map(|r| r.size as usize)
            .sum();
        self.requested_size -= size_to_remove;
        assert!(self.requested_size >= 0);
        self.pending_requests
            .retain(|request| self.requests_by_request.contains_key(request));
    }

    /// Ensures that pending requests don't increase the limits set by [max_pending] and
    /// [max_storage_area]: There can be at most [max_pending] requesting at most
    /// [max_storage_area] data.  Returns the new requests that have been added if any.
    pub fn update_pending_requests(
        &mut self,
        max_pending: usize,
        max_storage_area: usize,
    ) -> Vec<Request> {
        let max_files = 10;
        let free_slots = max_pending - self.pending_requests.len();
        let mut processed_requests: Vec<Request> = Default::default();
        let mut file_names: HashSet<String> = self
            .pending_requests
            .iter()
            .map(|x| x.name.clone())
            .collect();
        // The files are retrieved in alphabetic order.
        for (outgoing_request, (id, _)) in self.requests_by_request.iter() {
            if file_names.len() >= max_files && !file_names.contains(&outgoing_request.name) {
                break;
            }
            if processed_requests.len() >= free_slots {
                break;
            }
            if self.requested_size + (outgoing_request.size as usize) > max_storage_area {
                break;
            }
            if !self.pending_requests.contains(outgoing_request) {
                processed_requests.push(self.requests_by_id.get(id).unwrap().clone());
                self.pending_requests.insert(outgoing_request.clone());
                self.requested_size += (outgoing_request.size as usize);
                file_names.insert(outgoing_request.name.clone());
            }
        }

        debug!(
            "Requests to send out: {:?}",
            processed_requests.iter().map(|x| x.id).collect::<Vec<_>>()
        );

        debug!("pending_requests: {}", self.pending_requests.len(),);

        processed_requests
    }
}
