// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashSet;
use std::sync::Arc;

use api::v1::meta::router_client::RouterClient;
use api::v1::meta::{CreateRequest, DeleteRequest, RouteRequest, RouteResponse};
use common_grpc::channel_manager::ChannelManager;
use snafu::{ensure, Location, OptionExt, ResultExt};
use tokio::sync::RwLock;
use tonic::transport::Channel;

use crate::client::{load_balance as lb, Id};
use crate::error;
use crate::error::Error::TonicStatus;
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Client {
    inner: Arc<RwLock<Inner>>,
}

impl Client {
    pub fn new(id: Id, channel_manager: ChannelManager) -> Self {
        let inner = Arc::new(RwLock::new(Inner {
            id,
            channel_manager,
            peers: vec![],
        }));

        Self { inner }
    }

    pub async fn start<U, A>(&mut self, urls: A) -> Result<()>
    where
        U: AsRef<str>,
        A: AsRef<[U]>,
    {
        let mut inner = self.inner.write().await;
        inner.start(urls).await
    }

    pub async fn is_started(&self) -> bool {
        let inner = self.inner.read().await;
        inner.is_started()
    }

    pub async fn create(&self, req: CreateRequest) -> Result<RouteResponse> {
        let inner = self.inner.read().await;
        inner.create(req).await
    }

    pub async fn route(&self, req: RouteRequest) -> Result<RouteResponse> {
        let inner = self.inner.read().await;
        inner.route(req).await
    }

    pub async fn delete(&self, req: DeleteRequest) -> Result<RouteResponse> {
        let inner = self.inner.read().await;
        inner.delete(req).await
    }
}

#[derive(Debug)]
struct Inner {
    id: Id,
    channel_manager: ChannelManager,
    peers: Vec<String>,
}

impl Inner {
    async fn start<U, A>(&mut self, urls: A) -> Result<()>
    where
        U: AsRef<str>,
        A: AsRef<[U]>,
    {
        ensure!(
            !self.is_started(),
            error::IllegalGrpcClientStateSnafu {
                err_msg: "Router client already started",
            }
        );

        self.peers = urls
            .as_ref()
            .iter()
            .map(|url| url.as_ref().to_string())
            .collect::<HashSet<_>>()
            .drain()
            .collect::<Vec<_>>();

        Ok(())
    }

    async fn create(&self, mut req: CreateRequest) -> Result<RouteResponse> {
        let mut client = self.random_client()?;
        req.set_header(self.id);
        let res = client.create(req).await.context(error::TonicStatusSnafu)?;

        Ok(res.into_inner())
    }

    async fn route(&self, mut req: RouteRequest) -> Result<RouteResponse> {
        let mut client = self.random_client()?;
        req.set_header(self.id);
        let res = client.route(req).await.context(error::TonicStatusSnafu)?;

        Ok(res.into_inner())
    }

    async fn delete(&self, mut req: DeleteRequest) -> Result<RouteResponse> {
        let mut client = self.random_client()?;
        req.set_header(self.id);
        let res = client.delete(req).await.map_err(|mut source| {
            // FIXME(hl): here intentionally clear the metadata field so that error date does not changes which will break sqlness test.
            // we can remove this hack as soon as either: sqlness supports regex result match or greptimedb supports renaming table routes
            source.metadata_mut().clear();
            TonicStatus {
                source,
                location: Location::default(),
            }
        })?;
        Ok(res.into_inner())
    }

    fn random_client(&self) -> Result<RouterClient<Channel>> {
        let len = self.peers.len();
        let peer = lb::random_get(len, |i| Some(&self.peers[i])).context(
            error::IllegalGrpcClientStateSnafu {
                err_msg: "Empty peers, router client may not start yet",
            },
        )?;

        self.make_client(peer)
    }

    fn make_client(&self, addr: impl AsRef<str>) -> Result<RouterClient<Channel>> {
        let channel = self
            .channel_manager
            .get(addr)
            .context(error::CreateChannelSnafu)?;

        Ok(RouterClient::new(channel))
    }

    #[inline]
    fn is_started(&self) -> bool {
        !self.peers.is_empty()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_start_client() {
        let mut client = Client::new((0, 0), ChannelManager::default());
        assert!(!client.is_started().await);
        client
            .start(&["127.0.0.1:1000", "127.0.0.1:1001"])
            .await
            .unwrap();
        assert!(client.is_started().await);
    }

    #[tokio::test]
    async fn test_already_start() {
        let mut client = Client::new((0, 0), ChannelManager::default());
        client
            .start(&["127.0.0.1:1000", "127.0.0.1:1001"])
            .await
            .unwrap();
        assert!(client.is_started().await);
        let res = client.start(&["127.0.0.1:1002"]).await;
        assert!(res.is_err());
        assert!(matches!(
            res.err(),
            Some(error::Error::IllegalGrpcClientState { .. })
        ));
    }

    #[tokio::test]
    async fn test_start_with_duplicate_peers() {
        let mut client = Client::new((0, 0), ChannelManager::default());
        client
            .start(&["127.0.0.1:1000", "127.0.0.1:1000", "127.0.0.1:1000"])
            .await
            .unwrap();

        assert_eq!(1, client.inner.write().await.peers.len());
    }
}
