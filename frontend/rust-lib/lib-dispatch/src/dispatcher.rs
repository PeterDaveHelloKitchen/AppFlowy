use std::any::Any;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{future::Future, sync::Arc};

use derivative::*;
use pin_project::pin_project;
use tracing::event;

use crate::module::AFPluginStateMap;
use crate::runtime::AFPluginRuntime;
use crate::{
  errors::{DispatchError, Error, InternalError},
  module::{as_plugin_map, AFPlugin, AFPluginMap, AFPluginRequest},
  response::AFPluginEventResponse,
  service::{AFPluginServiceFactory, Service},
};

#[cfg(target_arch = "wasm32")]
pub trait AFConcurrent {}

#[cfg(target_arch = "wasm32")]
impl<T> AFConcurrent for T where T: ?Sized {}

#[cfg(not(target_arch = "wasm32"))]
pub trait AFConcurrent: Send + Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T> AFConcurrent for T where T: Send + Sync {}

#[cfg(target_arch = "wasm32")]
pub type AFBoxFuture<'a, T> = futures_core::future::LocalBoxFuture<'a, T>;

#[cfg(not(target_arch = "wasm32"))]
pub type AFBoxFuture<'a, T> = futures_core::future::BoxFuture<'a, T>;

pub type AFStateMap = std::sync::Arc<AFPluginStateMap>;

#[cfg(target_arch = "wasm32")]
pub(crate) fn downcast_owned<T: 'static>(boxed: AFBox) -> Option<T> {
  boxed.downcast().ok().map(|boxed| *boxed)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn downcast_owned<T: 'static + Send + Sync>(boxed: AFBox) -> Option<T> {
  boxed.downcast().ok().map(|boxed| *boxed)
}

#[cfg(target_arch = "wasm32")]
pub(crate) type AFBox = Box<dyn Any>;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type AFBox = Box<dyn Any + Send + Sync>;

#[cfg(target_arch = "wasm32")]
pub type BoxFutureCallback =
  Box<dyn FnOnce(AFPluginEventResponse) -> AFBoxFuture<'static, ()> + 'static>;

#[cfg(not(target_arch = "wasm32"))]
pub type BoxFutureCallback =
  Box<dyn FnOnce(AFPluginEventResponse) -> AFBoxFuture<'static, ()> + Send + Sync + 'static>;

#[cfg(target_arch = "wasm32")]
pub fn af_spawn<T>(future: T) -> tokio::task::JoinHandle<T::Output>
where
  T: Future + 'static,
  T::Output: 'static,
{
  tokio::task::spawn_local(future)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn af_spawn<T>(future: T) -> tokio::task::JoinHandle<T::Output>
where
  T: Future + Send + 'static,
  T::Output: Send + 'static,
{
  tokio::spawn(future)
}

pub struct AFPluginDispatcher {
  plugins: AFPluginMap,
  runtime: Arc<AFPluginRuntime>,
}

impl AFPluginDispatcher {
  pub fn new(runtime: Arc<AFPluginRuntime>, plugins: Vec<AFPlugin>) -> AFPluginDispatcher {
    tracing::trace!("{}", plugin_info(&plugins));
    AFPluginDispatcher {
      plugins: as_plugin_map(plugins),
      runtime,
    }
  }

  pub async fn async_send<Req>(dispatch: &AFPluginDispatcher, request: Req) -> AFPluginEventResponse
  where
    Req: Into<AFPluginRequest>,
  {
    AFPluginDispatcher::async_send_with_callback(dispatch, request, |_| Box::pin(async {})).await
  }

  pub async fn async_send_with_callback<Req, Callback>(
    dispatch: &AFPluginDispatcher,
    request: Req,
    callback: Callback,
  ) -> AFPluginEventResponse
  where
    Req: Into<AFPluginRequest>,
    Callback: FnOnce(AFPluginEventResponse) -> AFBoxFuture<'static, ()> + AFConcurrent + 'static,
  {
    let request: AFPluginRequest = request.into();
    let plugins = dispatch.plugins.clone();
    let service = Box::new(DispatchService { plugins });
    tracing::trace!("Async event: {:?}", &request.event);
    let service_ctx = DispatchContext {
      request,
      callback: Some(Box::new(callback)),
    };

    // Spawns a future onto the runtime.
    //
    // This spawns the given future onto the runtime's executor, usually a
    // thread pool. The thread pool is then responsible for polling the future
    // until it completes.
    //
    // The provided future will start running in the background immediately
    // when `spawn` is called, even if you don't await the returned
    // `JoinHandle`.
    let handle = dispatch.runtime.spawn(async move {
      service.call(service_ctx).await.unwrap_or_else(|e| {
        tracing::error!("Dispatch runtime error: {:?}", e);
        InternalError::Other(format!("{:?}", e)).as_response()
      })
    });

    let result = dispatch.runtime.run_until(handle).await;
    result.unwrap_or_else(|e| {
      let msg = format!("EVENT_DISPATCH join error: {:?}", e);
      tracing::error!("{}", msg);
      let error = InternalError::JoinError(msg);
      error.as_response()
    })
  }

  pub fn box_async_send<Req>(
    dispatch: &AFPluginDispatcher,
    request: Req,
  ) -> DispatchFuture<AFPluginEventResponse>
  where
    Req: Into<AFPluginRequest> + 'static,
  {
    AFPluginDispatcher::boxed_async_send_with_callback(dispatch, request, |_| Box::pin(async {}))
  }

  pub fn boxed_async_send_with_callback<Req, Callback>(
    dispatch: &AFPluginDispatcher,
    request: Req,
    callback: Callback,
  ) -> DispatchFuture<AFPluginEventResponse>
  where
    Req: Into<AFPluginRequest> + 'static,
    Callback: FnOnce(AFPluginEventResponse) -> AFBoxFuture<'static, ()> + AFConcurrent + 'static,
  {
    let request: AFPluginRequest = request.into();
    let plugins = dispatch.plugins.clone();
    let service = Box::new(DispatchService { plugins });
    tracing::trace!("Async event: {:?}", &request.event);
    let service_ctx = DispatchContext {
      request,
      callback: Some(Box::new(callback)),
    };

    // Spawns a future onto the runtime.
    //
    // This spawns the given future onto the runtime's executor, usually a
    // thread pool. The thread pool is then responsible for polling the future
    // until it completes.
    //
    // The provided future will start running in the background immediately
    // when `spawn` is called, even if you don't await the returned
    // `JoinHandle`.
    let handle = dispatch.runtime.spawn(async move {
      service.call(service_ctx).await.unwrap_or_else(|e| {
        tracing::error!("Dispatch runtime error: {:?}", e);
        InternalError::Other(format!("{:?}", e)).as_response()
      })
    });

    let runtime = dispatch.runtime.clone();
    DispatchFuture {
      fut: Box::pin(async move {
        let result = runtime.run_until(handle).await;
        result.unwrap_or_else(|e| {
          let msg = format!("EVENT_DISPATCH join error: {:?}", e);
          tracing::error!("{}", msg);
          let error = InternalError::JoinError(msg);
          error.as_response()
        })
      }),
    }
  }

  #[cfg(not(target_arch = "wasm32"))]
  pub fn sync_send(
    dispatch: Arc<AFPluginDispatcher>,
    request: AFPluginRequest,
  ) -> AFPluginEventResponse {
    futures::executor::block_on(AFPluginDispatcher::async_send_with_callback(
      dispatch.as_ref(),
      request,
      |_| Box::pin(async {}),
    ))
  }

  #[cfg(target_arch = "wasm32")]
  #[track_caller]
  pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
  where
    F: Future + 'static,
  {
    self.runtime.spawn(future)
  }

  #[cfg(not(target_arch = "wasm32"))]
  #[track_caller]
  pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
  where
    F: Future + Send + 'static,
    <F as Future>::Output: Send + 'static,
  {
    self.runtime.spawn(future)
  }

  #[cfg(target_arch = "wasm32")]
  pub async fn run_until<F>(&self, future: F) -> F::Output
  where
    F: Future + 'static,
  {
    let handle = self.runtime.spawn(future);
    self.runtime.run_until(handle).await.unwrap()
  }

  #[cfg(not(target_arch = "wasm32"))]
  pub async fn run_until<'a, F>(&self, future: F) -> F::Output
  where
    F: Future + Send + 'a,
    <F as Future>::Output: Send + 'a,
  {
    self.runtime.run_until(future).await
  }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct DispatchContext {
  pub request: AFPluginRequest,
  #[derivative(Debug = "ignore")]
  pub callback: Option<BoxFutureCallback>,
}

impl DispatchContext {
  pub(crate) fn into_parts(self) -> (AFPluginRequest, Option<BoxFutureCallback>) {
    let DispatchContext { request, callback } = self;
    (request, callback)
  }
}

pub(crate) struct DispatchService {
  pub(crate) plugins: AFPluginMap,
}

impl Service<DispatchContext> for DispatchService {
  type Response = AFPluginEventResponse;
  type Error = DispatchError;
  type Future = AFBoxFuture<'static, Result<Self::Response, Self::Error>>;

  #[tracing::instrument(name = "DispatchService", level = "debug", skip(self, ctx))]
  fn call(&self, ctx: DispatchContext) -> Self::Future {
    let module_map = self.plugins.clone();
    let (request, callback) = ctx.into_parts();

    Box::pin(async move {
      let result = {
        match module_map.get(&request.event) {
          Some(module) => {
            event!(
              tracing::Level::TRACE,
              "Handle event: {:?} by {:?}",
              &request.event,
              module.name
            );
            let fut = module.new_service(());
            let service_fut = fut.await?.call(request);
            service_fut.await
          },
          None => {
            let msg = format!("Can not find the event handler. {:?}", request);
            event!(tracing::Level::ERROR, "{}", msg);
            Err(InternalError::HandleNotFound(msg).into())
          },
        }
      };

      let response = result.unwrap_or_else(|e| e.into());
      event!(tracing::Level::TRACE, "Dispatch result: {:?}", response);
      if let Some(callback) = callback {
        callback(response.clone()).await;
      }

      Ok(response)
    })
  }
}

#[allow(dead_code)]
fn plugin_info(plugins: &[AFPlugin]) -> String {
  let mut info = format!("{} plugins loaded\n", plugins.len());
  for module in plugins {
    info.push_str(&format!("-> {} loaded \n", module.name));
  }
  info
}

#[allow(dead_code)]
fn print_plugins(plugins: &AFPluginMap) {
  plugins.iter().for_each(|(k, v)| {
    tracing::info!("Event: {:?} plugin : {:?}", k, v.name);
  })
}

#[pin_project]
pub struct DispatchFuture<T: AFConcurrent> {
  #[pin]
  pub fut: Pin<Box<dyn Future<Output = T> + 'static>>,
}

impl<T> Future for DispatchFuture<T>
where
  T: AFConcurrent + 'static,
{
  type Output = T;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = self.as_mut().project();
    Poll::Ready(futures_core::ready!(this.fut.poll(cx)))
  }
}
