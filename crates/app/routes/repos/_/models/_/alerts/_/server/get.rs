use crate::page::Page;
use anyhow::{bail, Result};
use pinwheel::prelude::*;
use std::{str::FromStr, sync::Arc};
use tangram_app_context::Context;
use tangram_app_core::{
	error::{bad_request, not_found, redirect_to_login, service_unavailable},
	path_components,
	user::{authorize_user, authorize_user_for_model},
};
use tangram_app_layouts::model_layout::{model_layout_info, ModelNavItem};
use tangram_id::Id;
use tracing::error;

pub async fn get(request: &mut http::Request<hyper::Body>) -> Result<http::Response<hyper::Body>> {
	let context = request.extensions().get::<Arc<Context>>().unwrap().clone();
	let app = &context.app;
	let (model_id, alert_id) = if let ["repos", _, "models", model_id, "alerts", alert_id] =
		path_components(request).as_slice()
	{
		(model_id.to_owned(), alert_id.to_owned())
	} else {
		bail!("unexpected path");
	};
	let model_id: Id = match model_id.parse() {
		Ok(model_id) => model_id,
		Err(_) => return Ok(bad_request()),
	};
	let mut db = match app.begin_transaction().await {
		Ok(db) => db,
		Err(_) => return Ok(service_unavailable()),
	};
	let user = match authorize_user(request, &mut db, app.options().auth_enabled()).await? {
		Ok(user) => user,
		Err(_) => return Ok(redirect_to_login()),
	};
	if !authorize_user_for_model(&mut db, &user, model_id).await? {
		return Ok(not_found());
	}
	let model_layout_info = model_layout_info(&mut db, app, model_id, ModelNavItem::Alerts).await?;
	let alert = app.get_alert(&mut db, Id::from_str(alert_id)?).await?;
	if alert.is_none() {
		error!("Alert {} not found in database", alert_id);
		return Ok(bad_request());
	}
	let page = Page {
		alert: alert.unwrap(),
		alert_id: alert_id.to_string(),
		model_layout_info,
		error: None,
	};
	app.commit_transaction(db).await?;
	let html = html(page);
	let response = http::Response::builder()
		.status(http::StatusCode::OK)
		.body(hyper::Body::from(html))
		.unwrap();
	Ok(response)
}