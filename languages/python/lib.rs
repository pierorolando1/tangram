use anyhow::anyhow;
use arrow2::{array::ArrayRef, ffi};
use memmap::Mmap;
use pyo3::{ffi::Py_uintptr_t, prelude::*, type_object::PyTypeObject, types::PyType, FromPyObject};
use std::collections::BTreeMap;
use url::Url;

/**
*/
#[pymodule]
#[pyo3(name = "tangram_python")]
fn tangram(py: Python, m: &PyModule) -> PyResult<()> {
	m.add_class::<LoadModelOptions>()?;
	m.add_class::<Model>()?;
	m.add_class::<RegressionMetrics>()?;
	m.add_class::<BinaryClassificationMetrics>()?;
	m.add_class::<MulticlassClassificationMetrics>()?;
	m.add_class::<PredictOptions>()?;
	m.add_class::<RegressionPredictOutput>()?;
	m.add_class::<BinaryClassificationPredictOutput>()?;
	m.add_class::<MulticlassClassificationPredictOutput>()?;
	m.add_class::<FeatureContributions>()?;
	m.add_class::<IdentityFeatureContribution>()?;
	m.add_class::<NormalizedFeatureContribution>()?;
	m.add_class::<OneHotEncodedFeatureContribution>()?;
	m.add_class::<BagOfWordsFeatureContribution>()?;
	m.add_class::<BagOfWordsCosineSimilarityFeatureContribution>()?;
	m.add_class::<WordEmbeddingFeatureContribution>()?;
	m.add_function(wrap_pyfunction!(train_inner, m)?)?;
	m.add("PredictInput", predict_input(py)?)?;
	m.add("PredictOutput", predict_output(py)?)?;
	m.add("FeatureContributionEntry", feature_contribution_entry(py)?)?;
	m.add("NGram", ngram(py)?)?;
	Ok(())
}

/**
Use this class to load a model, make predictions, and log events to the app.
*/
#[pyclass]
#[derive(Debug)]
struct Model {
	model: tangram_core::predict::Model,
	log_queue: Vec<Event>,
	tangram_url: Url,
	core_model: CoreModel,
}

#[derive(Debug)]
enum CoreModel {
	Path(String),
	Model(tangram_core::model::Model),
	Bytes(Vec<u8>),
}

#[pymethods]
impl Model {
	/**
	Load a model from a `.tangram` file at `path`.

	Args:
		path (str): The path to the `.tangram` file.
		options (Optional[`LoadModelOptions`]): The options to use when loading the model.

	Returns:
		model (`Model`)
	*/
	#[classmethod]
	#[args(options = "None")]
	#[pyo3(text_signature = "(path, options=None)")]
	fn from_path(
		_cls: &PyType,
		path: String,
		options: Option<LoadModelOptions>,
	) -> PyResult<Model> {
		let file = std::fs::File::open(&path)?;
		let bytes = unsafe { Mmap::map(&file)? };
		let model = tangram_model::from_bytes(&bytes).map_err(TangramError)?;
		let model = tangram_core::predict::Model::from(model);
		let tangram_url = options
			.and_then(|options| options.tangram_url)
			.unwrap_or_else(|| "https://app.tangram.dev".to_owned());
		let tangram_url = tangram_url
			.parse()
			.map_err(|_| TangramError(anyhow!("Failed to parse tangram_url")))?;
		let model = Model {
			model,
			log_queue: Vec::new(),
			tangram_url,
			core_model: CoreModel::Path(path),
		};
		Ok(model)
	}

	/**
	Load a model from bytes instead of a file. You should use this only if you already have a `.tangram` loaded into memory. Otherwise, use `Model.from_path`, which is faster because it memory maps the file.

	Args:
		bytes (str): The path to the `.tangram` file.
		options (Optional[`LoadModelOptions`]): The options to use when loading the model.

	Returns:
		model (`Model`)
	*/
	#[classmethod]
	#[args(options = "None")]
	#[pyo3(text_signature = "(bytes, options=None)")]
	fn from_bytes(
		_cls: &PyType,
		bytes: Vec<u8>,
		options: Option<LoadModelOptions>,
	) -> PyResult<Model> {
		let model = tangram_model::from_bytes(&bytes).map_err(TangramError)?;
		let model = tangram_core::predict::Model::from(model);
		let tangram_url = options
			.and_then(|options| options.tangram_url)
			.unwrap_or_else(|| "https://app.tangram.dev".to_owned());
		let tangram_url = tangram_url
			.parse()
			.map_err(|_| TangramError(anyhow!("Failed to parse tangram_url")))?;
		let model = Model {
			model,
			log_queue: Vec::new(),
			tangram_url,
			core_model: CoreModel::Bytes(bytes),
		};
		Ok(model)
	}

	/**
	Retrieve the model's id.
		*/
	#[getter]
	fn id(&self) -> String {
		self.model.id.clone()
	}

	/**
	Set the model's tangram_url.
		*/
	#[setter]
	fn set_tangram_url(&mut self, url: String) -> PyResult<()> {
		let tangram_url = url
			.parse()
			.map_err(|_| TangramError(anyhow!("Failed to parse tangram_url")))?;
		self.tangram_url = tangram_url;
		Ok(())
	}

	#[pyo3(text_signature = "(path)")]
	fn to_path(&self, path: String) -> PyResult<()> {
		let path: std::path::PathBuf = path.into();
		match &self.core_model {
			CoreModel::Model(model) => model
				.to_path(&path)
				.map_err(|err| TangramError(err.into()))?,
			CoreModel::Path(current_model_path) => std::fs::copy(current_model_path, path)
				.map_err(|err| TangramError(err.into()))
				.map(|_| {})?,
			CoreModel::Bytes(bytes) => {
				tangram_model::to_path(&path, &bytes).map_err(|err| TangramError(err.into()))?;
			}
		};
		Ok(())
	}

	/**
	Make a prediction!

	Args:
		input (Union[List[`PredictInput`], `PredictInput`]): A predict input is either a single predict input which is a dict from strings to strings or floats or an array of such dicts. The keys should match the columns in the CSV file you trained your model with.
		options (Optional[`PredictOptions`]): These are the predict options.

	Returns:
		[Union[List[`PredictOutput`], `PredictOutput`]). Return a single output if `input` was a single input, or an array if `input` was an array of `input`s.
	*/
	#[pyo3(text_signature = "(input, options=None)")]
	fn predict(
		&self,
		input: PredictInputSingleOrMultiple,
		options: Option<&PredictOptions>,
	) -> PredictOutputSingleOrMultiple {
		let model = &self.model;
		let options = options.map(Into::into).unwrap_or_default();
		match input {
			PredictInputSingleOrMultiple::Single(input) => {
				let input = input.into();
				let mut output = tangram_core::predict::predict(model, &[input], &options);
				let output = output.remove(0);
				let output = output.into();
				PredictOutputSingleOrMultiple::Single(output)
			}
			PredictInputSingleOrMultiple::Multiple(input) => {
				let input = input.into_iter().map(Into::into).collect::<Vec<_>>();
				let output = tangram_core::predict::predict(model, &input, &options);
				let output = output.into_iter().map(Into::into).collect();
				PredictOutputSingleOrMultiple::Multiple(output)
			}
		}
	}

	/**
	Send a prediction event to the app. If you want to batch events, you can use `enqueue_log_prediction` instead.

	Args:
		identifier (Union[str, float]): This is a unique identifier for the prediction, which will associate it with a true value event and allow you to look it up in the app.
		input (`PredictInput`): A single `PredictInput`.
		output (`PredictOutput`): A single `PredictOutput`.
		options (Optional[`PredictOptions`]): This is the same `PredictOptions` value that you passed to `predict`.
	  */
	#[args(identifier, input, output, options = "None")]
	#[pyo3(text_signature = "(identifier, input, output, options=None)")]
	fn log_prediction(
		&mut self,
		identifier: NumberOrString,
		input: PredictInput,
		output: PredictOutput,
		options: Option<PredictOptions>,
	) -> PyResult<()> {
		let event = Event::Prediction(self.prediction_event(identifier, input, output, options));
		self.log_event(event)?;
		Ok(())
	}

	/**
	Add a prediction event to the queue. Remember to call `flush_log_queue` at a later point to send the event to the app.
	Args:
		identifier (Union[str, float]): This is a unique identifier for the prediction, which will associate it with a true value event and allow you to look it up in the app.
		input (`PredictInput`): A single `PredictInput`.
		output (`PredictOutput`): A single `PredictOutput`.
		options (`PredictOptions`): This is the same `predictOptions` value that you passed to `predict`.
	*/
	#[pyo3(text_signature = "(identifier, input, output, options=None)")]
	fn enqueue_log_prediction(
		&mut self,
		identifier: NumberOrString,
		input: PredictInput,
		output: PredictOutput,
		options: Option<PredictOptions>,
	) {
		let event = Event::Prediction(self.prediction_event(identifier, input, output, options));
		self.log_queue.push(event);
	}

	/**
	Send a true value event to the app. If you want to batch events, you can use `enqueue_log_true_value` instead.

	Args:
		identifier (Union[str, float]): This is a unique identifier for the prediction, which will associate it with a true value event and allow you to look it up in the app.
		true_value (Union[str, float]): This is the true value for the prediction.
	*/
	#[pyo3(text_signature = "(identifier, true_value)")]
	fn log_true_value(
		&mut self,
		identifier: NumberOrString,
		true_value: NumberOrString,
	) -> PyResult<()> {
		let event = Event::TrueValue(self.true_value_event(identifier, true_value));
		self.log_event(event)?;
		Ok(())
	}

	/**
	Add a true value event to the queue. Remember to call `flush_log_queue` at a later point to send the event to the app.

	Args:
		identifier (Union[str, float]): This is a unique identifier for the prediction, which will associate it with a true value event and allow you to look it up in the app.
		true_value (Union[str, float]): This is the true value for the prediction.
	*/
	#[pyo3(text_signature = "(identifier, true_value)")]
	fn enqueue_log_true_value(&mut self, identifier: NumberOrString, true_value: NumberOrString) {
		let event = Event::TrueValue(self.true_value_event(identifier, true_value));
		self.log_queue.push(event);
	}

	/**
	Send all events in the queue to the app.
	*/
	#[pyo3(text_signature = "()")]
	fn flush_log_queue(&mut self) -> PyResult<()> {
		let events = self.log_queue.drain(0..self.log_queue.len()).collect();
		self.log_events(events)
	}

	/**
	Retrieve the model's test metrics.
		*/
	fn test_metrics(&self) -> Metrics {
		match &self.core_model {
			CoreModel::Path(path) => test_metrics_from_path(path),
			CoreModel::Bytes(bytes) => test_metrics_from_bytes(bytes),
			CoreModel::Model(model) => test_metrics_from_model(model),
		}
	}
}

fn test_metrics_from_path(path: &str) -> Metrics {
	todo!()
}

fn test_metrics_from_bytes(bytes: &[u8]) -> Metrics {
	todo!()
}

fn test_metrics_from_model(model: &tangram_core::model::Model) -> Metrics {
	match &model.inner {
		tangram_core::model::ModelInner::Regressor(regressor) => {
			Metrics::Regression((&regressor.test_metrics).into())
		}
		tangram_core::model::ModelInner::BinaryClassifier(binary_classifier) => {
			Metrics::BinaryClassification((&binary_classifier.test_metrics).into())
		}
		tangram_core::model::ModelInner::MulticlassClassifier(multiclass_classifier) => {
			Metrics::MulticlassClassification((&multiclass_classifier.test_metrics).into())
		}
	}
}

impl Model {
	fn log_event(&mut self, event: Event) -> PyResult<()> {
		self.log_events(vec![event])
	}

	fn log_events(&mut self, events: Vec<Event>) -> PyResult<()> {
		let mut url = self.tangram_url.clone();
		url.set_path("/track");
		let body = serde_json::to_vec(&events).map_err(|err| TangramError(err.into()))?;
		reqwest::blocking::Client::new()
			.post(url)
			.body(body)
			.send()
			.map_err(|err| TangramError(err.into()))?;
		Ok(())
	}

	fn prediction_event(
		&self,
		identifier: NumberOrString,
		input: PredictInput,
		output: PredictOutput,
		options: Option<PredictOptions>,
	) -> PredictionEvent {
		PredictionEvent {
			date: chrono::Utc::now(),
			identifier,
			input,
			options,
			output,
			model_id: self.id(),
		}
	}

	fn true_value_event(
		&self,
		identifier: NumberOrString,
		true_value: NumberOrString,
	) -> TrueValueEvent {
		TrueValueEvent {
			date: chrono::Utc::now(),
			identifier,
			model_id: self.id(),
			true_value,
		}
	}
}

#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
enum Metrics {
	Regression(RegressionMetrics),
	BinaryClassification(BinaryClassificationMetrics),
	MulticlassClassification(MulticlassClassificationMetrics),
}

impl IntoPy<PyObject> for Metrics {
	fn into_py(self, py: Python) -> PyObject {
		match self {
			Metrics::Regression(s) => s.into_py(py),
			Metrics::BinaryClassification(s) => s.into_py(py),
			Metrics::MulticlassClassification(s) => s.into_py(py),
		}
	}
}

#[derive(Clone, Debug, serde::Serialize)]
#[pyclass]
pub struct RegressionMetrics {
	/// The mean squared error is equal to the mean of the squared errors. For a given example, the error is the difference between the true value and the model's predicted value.
	pub mse: f32,
	/// The root mean squared error is equal to the square root of the mean squared error.
	pub rmse: f32,
	/// The mean of the absolute value of the errors.
	pub mae: f32,
	/// The r-squared value. https://en.wikipedia.org/wiki/Coefficient_of_determination.
	pub r2: f32,
}

impl From<&tangram_metrics::RegressionMetricsOutput> for RegressionMetrics {
	fn from(_: &tangram_metrics::RegressionMetricsOutput) -> Self {
		todo!()
	}
}

impl From<&tangram_metrics::BinaryClassificationMetricsOutput> for BinaryClassificationMetrics {
	fn from(metrics: &tangram_metrics::BinaryClassificationMetricsOutput) -> Self {
		BinaryClassificationMetrics {
			auc_roc_approx: metrics.auc_roc_approx,
			thresholds: metrics
				.thresholds
				.iter()
				.map(Into::into)
				.collect::<Vec<_>>(),
		}
	}
}

impl From<&tangram_metrics::BinaryClassificationMetricsOutputForThreshold>
	for BinaryClassificationMetricsOutputForThreshold
{
	fn from(metrics: &tangram_metrics::BinaryClassificationMetricsOutputForThreshold) -> Self {
		BinaryClassificationMetricsOutputForThreshold {
			threshold: metrics.threshold,
			true_positives: metrics.true_positives,
			false_positives: metrics.false_positives,
			true_negatives: metrics.true_negatives,
			false_negatives: metrics.false_negatives,
			accuracy: metrics.accuracy,
			precision: metrics.precision,
			recall: metrics.recall,
			f1_score: metrics.f1_score,
			true_positive_rate: metrics.true_positive_rate,
			false_positive_rate: metrics.false_positive_rate,
		}
	}
}

impl From<&tangram_metrics::MulticlassClassificationMetricsOutput>
	for MulticlassClassificationMetrics
{
	fn from(_: &tangram_metrics::MulticlassClassificationMetricsOutput) -> Self {
		todo!()
	}
}

#[derive(Clone, Debug, serde::Serialize)]
#[pyclass]
/// BinaryClassificationMetrics contains common metrics used to evaluate binary classifiers.
pub struct BinaryClassificationMetrics {
	/// The area under the receiver operating characteristic curve is computed using a fixed number of thresholds equal to `n_thresholds`.
	pub auc_roc_approx: f32,
	/// This contains metrics specific to each classification threshold.
	pub thresholds: Vec<BinaryClassificationMetricsOutputForThreshold>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[pyclass]
pub struct BinaryClassificationMetricsOutputForThreshold {
	/// The classification threshold.
	pub threshold: f32,
	/// The total number of examples whose label is equal to the positive class that the model predicted as belonging to the positive class.
	pub true_positives: u64,
	/// The total number of examples whose label is equal to the negative class that the model predicted as belonging to the positive class.
	pub false_positives: u64,
	/// The total number of examples whose label is equal to the negative class that the model predicted as belonging to the negative class.
	pub true_negatives: u64,
	/// The total number of examples whose label is equal to the positive class that the model predicted as belonging to the negative class.
	pub false_negatives: u64,
	/// The fraction of examples that were correctly classified.
	pub accuracy: f32,
	/// The precision is the fraction of examples the model predicted as belonging to the positive class whose label is actually the positive class. true_positives / (true_positives + false_positives). See [Precision and Recall](https://en.wikipedia.org/wiki/Precision_and_recall).
	pub precision: Option<f32>,
	/// The recall is the fraction of examples whose label is equal to the positive class that the model predicted as belonging to the positive class. `recall = true_positives / (true_positives + false_negatives)`.
	pub recall: Option<f32>,
	/// The f1 score is the harmonic mean of the precision and the recall. See [F1 Score](https://en.wikipedia.org/wiki/F1_score).
	pub f1_score: Option<f32>,
	/// The true positive rate is the fraction of examples whose label is equal to the positive class that the model predicted as belonging to the positive class. Also known as the recall. See [Sensitivity and Specificity](https://en.wikipedia.org/wiki/Sensitivity_and_specificity).
	pub true_positive_rate: f32,
	/// The false positive rate is the fraction of examples whose label is equal to the negative class that the model falsely predicted as belonging to the positive class. false_positives / (false_positives + true_negatives). See [False Positive Rate](https://en.wikipedia.org/wiki/False_positive_rate)
	pub false_positive_rate: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[pyclass]
struct MulticlassClassificationMetrics {
	/// The class metrics contain class specific metrics.
	pub class_metrics: Vec<ClassMetrics>,
	/// The accuracy is the fraction of all of the predictions that are correct.
	pub accuracy: f32,
	/// The unweighted precision equal to the mean of each class's precision.
	pub precision_unweighted: f32,
	/// The weighted precision is a weighted mean of each class's precision weighted by the fraction of the total examples in the class.
	pub precision_weighted: f32,
	/// The unweighted recall equal to the mean of each class's recall.
	pub recall_unweighted: f32,
	/// The weighted recall is a weighted mean of each class's recall weighted by the fraction of the total examples in the class.
	pub recall_weighted: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
/// ClassMetrics are class specific metrics used to evaluate the model's performance on each individual class.
pub struct ClassMetrics {
	/// This is the total number of examples whose label is equal to this class that the model predicted as belonging to this class.
	pub true_positives: u64,
	/// This is the total number of examples whose label is *not* equal to this class that the model predicted as belonging to this class.
	pub false_positives: u64,
	/// This is the total number of examples whose label is *not* equal to this class that the model predicted as *not* belonging to this class.
	pub true_negatives: u64,
	/// This is the total number of examples whose label is equal to this class that the model predicted as *not* belonging to this class.
	pub false_negatives: u64,
	/// The accuracy is the fraction of examples of this class that were correctly classified.
	pub accuracy: f32,
	/// The precision is the fraction of examples the model predicted as belonging to this class whose label is actually equal to this class. `precision = true_positives / (true_positives + false_positives)`. See [Precision and Recall](https://en.wikipedia.org/wiki/Precision_and_recall).
	pub precision: f32,
	/// The recall is the fraction of examples in the dataset whose label is equal to this class that the model predicted as equal to this class. `recall = true_positives / (true_positives + false_negatives)`.
	pub recall: f32,
	/// The f1 score is the harmonic mean of the precision and the recall. See [F1 Score](https://en.wikipedia.org/wiki/F1_score).
	pub f1_score: f32,
}

/**
These are the options passed when loading a model.

Attributes:
	tangram_url (Optional[str]): If you are running the app locally or on your own server, use this field to provide the url to it.
*/
#[pyclass]
#[derive(Clone, Debug)]
struct LoadModelOptions {
	#[pyo3(get, set)]
	tangram_url: Option<String>,
}

#[pymethods]
impl LoadModelOptions {
	#[new]
	fn new(tangram_url: Option<String>) -> LoadModelOptions {
		LoadModelOptions { tangram_url }
	}
}

#[derive(FromPyObject)]
enum PredictInputSingleOrMultiple {
	Single(PredictInput),
	Multiple(PredictInputMultiple),
}

#[derive(Debug, FromPyObject, serde::Serialize)]
struct PredictInput(BTreeMap<String, PredictInputValue>);

type PredictInputMultiple = Vec<PredictInput>;

impl From<PredictInput> for tangram_core::predict::PredictInput {
	fn from(value: PredictInput) -> tangram_core::predict::PredictInput {
		tangram_core::predict::PredictInput(
			value
				.0
				.into_iter()
				.map(|(key, value)| (key, value.into()))
				.collect(),
		)
	}
}

#[derive(Debug, FromPyObject, serde::Serialize)]
#[serde(untagged)]
enum PredictInputValue {
	Number(f64),
	String(String),
}

impl From<PredictInputValue> for tangram_core::predict::PredictInputValue {
	fn from(value: PredictInputValue) -> tangram_core::predict::PredictInputValue {
		match value {
			PredictInputValue::Number(value) => {
				tangram_core::predict::PredictInputValue::Number(value)
			}
			PredictInputValue::String(value) => {
				tangram_core::predict::PredictInputValue::String(value)
			}
		}
	}
}

/**
These are the options passed to `Model.predict`.

Attributes:
	threshold (Optional[float]): If your model is a binary classifier, use this field to make predictions using a threshold chosen on the tuning page of the app. The default value is `0.5`.

	compute_feature_contributions (Optional[bool]): Computing feature contributions is disabled by default. If you set this field to `true`, you will be able to access the feature contributions with the `featureContributions` field of the predict output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct PredictOptions {
	#[pyo3(get, set)]
	threshold: Option<f32>,
	#[pyo3(get, set)]
	compute_feature_contributions: Option<bool>,
}

#[pymethods]
impl PredictOptions {
	#[new]
	fn new(threshold: Option<f32>, compute_feature_contributions: Option<bool>) -> PredictOptions {
		PredictOptions {
			threshold,
			compute_feature_contributions,
		}
	}
}

impl From<&PredictOptions> for tangram_core::predict::PredictOptions {
	fn from(value: &PredictOptions) -> tangram_core::predict::PredictOptions {
		let mut options = tangram_core::predict::PredictOptions::default();
		if let Some(threshold) = value.threshold {
			options.threshold = threshold;
		}
		if let Some(compute_feature_contributions) = value.compute_feature_contributions {
			options.compute_feature_contributions = compute_feature_contributions;
		}
		options
	}
}

enum PredictOutputSingleOrMultiple {
	Single(PredictOutput),
	Multiple(PredictOutputMultiple),
}

impl IntoPy<PyObject> for PredictOutputSingleOrMultiple {
	fn into_py(self, py: Python) -> PyObject {
		match self {
			PredictOutputSingleOrMultiple::Single(s) => s.into_py(py),
			PredictOutputSingleOrMultiple::Multiple(s) => s.into_py(py),
		}
	}
}

#[derive(Debug, serde::Serialize, FromPyObject)]
#[serde(untagged)]
enum PredictOutput {
	Regression(RegressionPredictOutput),
	BinaryClassification(BinaryClassificationPredictOutput),
	MulticlassClassification(MulticlassClassificationPredictOutput),
}

type PredictOutputMultiple = Vec<PredictOutput>;

impl IntoPy<PyObject> for PredictOutput {
	fn into_py(self, py: Python) -> PyObject {
		match self {
			PredictOutput::Regression(s) => s.into_py(py),
			PredictOutput::BinaryClassification(s) => s.into_py(py),
			PredictOutput::MulticlassClassification(s) => s.into_py(py),
		}
	}
}

impl From<tangram_core::predict::PredictOutput> for PredictOutput {
	fn from(value: tangram_core::predict::PredictOutput) -> Self {
		match value {
			tangram_core::predict::PredictOutput::Regression(value) => {
				PredictOutput::Regression(value.into())
			}
			tangram_core::predict::PredictOutput::BinaryClassification(value) => {
				PredictOutput::BinaryClassification(value.into())
			}
			tangram_core::predict::PredictOutput::MulticlassClassification(value) => {
				PredictOutput::MulticlassClassification(value.into())
			}
		}
	}
}

/**
`Model.predict` outputs `RegressionPredictOutput` when the model's task is regression.

Attributes:
	value: This is the predicted value.
	feature_contributions (`FeatureContributions`): If computing feature contributions was enabled in the predict options, this value will explain the model's output, showing how much each feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct RegressionPredictOutput {
	#[pyo3(get)]
	value: f32,
	#[pyo3(get)]
	#[serde(skip_serializing)]
	feature_contributions: Option<FeatureContributions>,
}

impl From<tangram_core::predict::RegressionPredictOutput> for RegressionPredictOutput {
	fn from(value: tangram_core::predict::RegressionPredictOutput) -> Self {
		RegressionPredictOutput {
			value: value.value,
			feature_contributions: value.feature_contributions.map(Into::into),
		}
	}
}

/**
`Model.predict` outputs `BinaryClassificationPredictOutput` when the model's task is binary classification.

Attributes:
	class_name (str): This is the name of the predicted class.
	probability (float): This is the probability the model assigned to the predicted class.
	feature_contributions (`FeatureContributions`): If computing feature contributions was enabled in the predict options, this value will explain the model's output, showing how much each feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct BinaryClassificationPredictOutput {
	#[pyo3(get)]
	class_name: String,
	#[pyo3(get)]
	probability: f32,
	#[pyo3(get)]
	#[serde(skip_serializing)]
	feature_contributions: Option<FeatureContributions>,
}

impl From<tangram_core::predict::BinaryClassificationPredictOutput>
	for BinaryClassificationPredictOutput
{
	fn from(value: tangram_core::predict::BinaryClassificationPredictOutput) -> Self {
		BinaryClassificationPredictOutput {
			class_name: value.class_name,
			probability: value.probability,
			feature_contributions: value.feature_contributions.map(Into::into),
		}
	}
}

/**
`Model.predict` outputs `MulticlassClassificationPredictOutput` when the model's task is multiclass classification.

Attributes:
	class_name (str): This is the name of the predicted class.
	probability (float): This is the probability the model assigned to the predicted class.
	probabilities (Dict[str, float]): This value maps from class names to the probability the model assigned to each class.
	feature_contributions (Dict[str, `FeatureContributions`]): If computing feature contributions was enabled in the predict options, this value will explain the model's output, showing how much each feature contributed to the output. This value maps from class names to `FeatureContributions` values for each class. The class with the `FeatureContributions` value with the highest `FeatureContributions.output_value` is the predicted class.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct MulticlassClassificationPredictOutput {
	#[pyo3(get)]
	class_name: String,
	#[pyo3(get)]
	probability: f32,
	#[pyo3(get)]
	probabilities: BTreeMap<String, f32>,
	#[pyo3(get)]
	#[serde(skip_serializing)]
	feature_contributions: Option<BTreeMap<String, FeatureContributions>>,
}

impl From<tangram_core::predict::MulticlassClassificationPredictOutput>
	for MulticlassClassificationPredictOutput
{
	fn from(value: tangram_core::predict::MulticlassClassificationPredictOutput) -> Self {
		MulticlassClassificationPredictOutput {
			class_name: value.class_name,
			probability: value.probability,
			probabilities: value.probabilities,
			feature_contributions: value.feature_contributions.map(|feature_contributions| {
				feature_contributions
					.into_iter()
					.map(|(key, value)| (key, value.into()))
					.collect()
			}),
		}
	}
}

/**
This is a description of the feature contributions for the prediction if the task is regression or binary classification, or for a single class if the task is multiclass classification.

Attributes:
	baseline_value (float): This is the value the model would output if all features had baseline values.
	output_value (float): This is the value the model output. Any difference from the `baseline_value` is because of the deviation of the features from their baseline values.
	entries (List[`FeatureContributionEntry`]): This list will contain one entry for each of the model's features. Note that features are computed from columns, so there will likely be more features than columns.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct FeatureContributions {
	#[pyo3(get)]
	baseline_value: f32,
	#[pyo3(get)]
	output_value: f32,
	#[pyo3(get)]
	entries: Vec<FeatureContributionEntry>,
}

impl From<tangram_core::predict::FeatureContributions> for FeatureContributions {
	fn from(value: tangram_core::predict::FeatureContributions) -> Self {
		FeatureContributions {
			baseline_value: value.baseline_value,
			output_value: value.output_value,
			entries: value.entries.into_iter().map(Into::into).collect(),
		}
	}
}

/// This identifies the type of a feature contribution.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type")]
enum FeatureContributionEntry {
	#[serde(rename = "identity")]
	Identity(IdentityFeatureContribution),
	#[serde(rename = "normalized")]
	Normalized(NormalizedFeatureContribution),
	#[serde(rename = "one_hot_encoded")]
	OneHotEncoded(OneHotEncodedFeatureContribution),
	#[serde(rename = "bag_of_words")]
	BagOfWords(BagOfWordsFeatureContribution),
	#[serde(rename = "bag_of_words_cosine_similarity")]
	BagOfWordsCosineSimilarity(BagOfWordsCosineSimilarityFeatureContribution),
	#[serde(rename = "word_embedding")]
	WordEmbedding(WordEmbeddingFeatureContribution),
}

impl IntoPy<PyObject> for FeatureContributionEntry {
	fn into_py(self, py: Python) -> PyObject {
		match self {
			FeatureContributionEntry::Identity(s) => s.into_py(py),
			FeatureContributionEntry::Normalized(s) => s.into_py(py),
			FeatureContributionEntry::OneHotEncoded(s) => s.into_py(py),
			FeatureContributionEntry::BagOfWords(s) => s.into_py(py),
			FeatureContributionEntry::BagOfWordsCosineSimilarity(s) => s.into_py(py),
			FeatureContributionEntry::WordEmbedding(s) => s.into_py(py),
		}
	}
}

impl From<tangram_core::predict::FeatureContributionEntry> for FeatureContributionEntry {
	fn from(value: tangram_core::predict::FeatureContributionEntry) -> Self {
		match value {
			tangram_core::predict::FeatureContributionEntry::Identity(value) => {
				FeatureContributionEntry::Identity(value.into())
			}
			tangram_core::predict::FeatureContributionEntry::Normalized(value) => {
				FeatureContributionEntry::Normalized(value.into())
			}
			tangram_core::predict::FeatureContributionEntry::OneHotEncoded(value) => {
				FeatureContributionEntry::OneHotEncoded(value.into())
			}
			tangram_core::predict::FeatureContributionEntry::BagOfWords(value) => {
				FeatureContributionEntry::BagOfWords(value.into())
			}
			tangram_core::predict::FeatureContributionEntry::BagOfWordsCosineSimilarity(value) => {
				FeatureContributionEntry::BagOfWordsCosineSimilarity(value.into())
			}
			tangram_core::predict::FeatureContributionEntry::WordEmbedding(value) => {
				FeatureContributionEntry::WordEmbedding(value.into())
			}
		}
	}
}

/**
This describes the contribution of a feature from an identity feature group.

Attributes:
	column_name (str): This is the name of the source column for the feature group.
	feature_value (float): This is the value of the feature.
	feature_contribution_value (float): This is the amount that the feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct IdentityFeatureContribution {
	#[pyo3(get)]
	column_name: String,
	#[pyo3(get)]
	feature_contribution_value: f32,
	#[pyo3(get)]
	feature_value: f32,
}

impl From<tangram_core::predict::IdentityFeatureContribution> for IdentityFeatureContribution {
	fn from(value: tangram_core::predict::IdentityFeatureContribution) -> Self {
		IdentityFeatureContribution {
			column_name: value.column_name,
			feature_contribution_value: value.feature_contribution_value,
			feature_value: value.feature_value,
		}
	}
}

/**
This describes the contribution of a feature from a normalized feature group.

Attributes:
	column_name (str): This is the name of the source column for the feature group.
	feature_value (float): This is the value of the feature.
	feature_contribution_value (float): This is the amount that the feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct NormalizedFeatureContribution {
	#[pyo3(get)]
	column_name: String,
	#[pyo3(get)]
	feature_value: f32,
	#[pyo3(get)]
	feature_contribution_value: f32,
}

impl From<tangram_core::predict::NormalizedFeatureContribution> for NormalizedFeatureContribution {
	fn from(value: tangram_core::predict::NormalizedFeatureContribution) -> Self {
		NormalizedFeatureContribution {
			column_name: value.column_name,
			feature_value: value.feature_value,
			feature_contribution_value: value.feature_contribution_value,
		}
	}
}

/**
This describes the contribution of a feature from a one hot encoded feature group.

Attributes:
	column_name (str): This is the name of the source column for the feature group.
	variant (str): This is the enum variant the feature indicates the presence of.
	feature_value (float): This is the value of the feature.
	feature_contribution_value (float): This is the amount that the feature contributed to the output.b
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct OneHotEncodedFeatureContribution {
	#[pyo3(get)]
	column_name: String,
	#[pyo3(get)]
	variant: Option<String>,
	#[pyo3(get)]
	feature_value: bool,
	#[pyo3(get)]
	feature_contribution_value: f32,
}

impl From<tangram_core::predict::OneHotEncodedFeatureContribution>
	for OneHotEncodedFeatureContribution
{
	fn from(value: tangram_core::predict::OneHotEncodedFeatureContribution) -> Self {
		OneHotEncodedFeatureContribution {
			column_name: value.column_name,
			variant: value.variant,
			feature_value: value.feature_value,
			feature_contribution_value: value.feature_contribution_value,
		}
	}
}

/**
This describes the contribution of a feature from a bag of words feature group.

Attributes:
	column_name (str): This is the name of the source column for the feature group.
	ngram (`NGram`): This is the ngram for the feature.
	feature_value (float): This is the value of the feature..
	feature_contribution_value (float): This is the amount that the feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct BagOfWordsFeatureContribution {
	#[pyo3(get)]
	column_name: String,
	#[pyo3(get)]
	ngram: NGram,
	#[pyo3(get)]
	feature_value: f32,
	#[pyo3(get)]
	feature_contribution_value: f32,
}

impl From<tangram_core::predict::BagOfWordsFeatureContribution> for BagOfWordsFeatureContribution {
	fn from(value: tangram_core::predict::BagOfWordsFeatureContribution) -> Self {
		BagOfWordsFeatureContribution {
			column_name: value.column_name,
			ngram: value.ngram.into(),
			feature_value: value.feature_value,
			feature_contribution_value: value.feature_contribution_value,
		}
	}
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(untagged)]
enum NGram {
	Unigram(String),
	Bigram(String, String),
}

impl IntoPy<PyObject> for NGram {
	fn into_py(self, py: Python) -> PyObject {
		match self {
			NGram::Unigram(token) => token.into_py(py),
			NGram::Bigram(token_a, token_b) => vec![token_a, token_b].into_py(py),
		}
	}
}

impl From<tangram_core::predict::NGram> for NGram {
	fn from(value: tangram_core::predict::NGram) -> Self {
		match value {
			tangram_core::predict::NGram::Unigram(token) => NGram::Unigram(token),
			tangram_core::predict::NGram::Bigram(token_a, token_b) => {
				NGram::Bigram(token_a, token_b)
			}
		}
	}
}

/**
This describes the contribution of a feature from a bag of words cosine similarity feature group.

Attributes:
	column_name_a (str): This is the name of the first source column for the feature group.
	column_name_b (str): This is the name of the second source column for the feature group.
	ngram (`NGram`): This is the ngram for the feature.
	feature_value (float): This is the value of the feature..
	feature_contribution_value (float): This is the amount that the feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct BagOfWordsCosineSimilarityFeatureContribution {
	#[pyo3(get)]
	column_name_a: String,
	#[pyo3(get)]
	column_name_b: String,
	#[pyo3(get)]
	feature_value: f32,
	#[pyo3(get)]
	feature_contribution_value: f32,
}

impl From<tangram_core::predict::BagOfWordsCosineSimilarityFeatureContribution>
	for BagOfWordsCosineSimilarityFeatureContribution
{
	fn from(value: tangram_core::predict::BagOfWordsCosineSimilarityFeatureContribution) -> Self {
		BagOfWordsCosineSimilarityFeatureContribution {
			column_name_a: value.column_name_a,
			column_name_b: value.column_name_b,
			feature_value: value.feature_value,
			feature_contribution_value: value.feature_contribution_value,
		}
	}
}

/**
This describes the contribution of a feature from a word embedding feature group.

Attributes:
	column_name (str): This is the name of the source column for the feature group.
	value_index (int): This is the index of the feature in the word embedding.
	feature_contribution_value (float): This is the amount that the feature contributed to the output.
*/
#[pyclass]
#[derive(Clone, Debug, serde::Serialize)]
struct WordEmbeddingFeatureContribution {
	#[pyo3(get)]
	column_name: String,
	#[pyo3(get)]
	value_index: usize,
	#[pyo3(get)]
	feature_contribution_value: f32,
}

impl From<tangram_core::predict::WordEmbeddingFeatureContribution>
	for WordEmbeddingFeatureContribution
{
	fn from(value: tangram_core::predict::WordEmbeddingFeatureContribution) -> Self {
		WordEmbeddingFeatureContribution {
			column_name: value.column_name,
			value_index: value.value_index,
			feature_contribution_value: value.feature_contribution_value,
		}
	}
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type")]
enum Event {
	#[serde(rename = "prediction")]
	Prediction(PredictionEvent),
	#[serde(rename = "true_value")]
	TrueValue(TrueValueEvent),
}

#[pyclass]
#[derive(Debug, serde::Serialize)]
struct PredictionEvent {
	date: chrono::DateTime<chrono::Utc>,
	identifier: NumberOrString,
	input: PredictInput,
	options: Option<PredictOptions>,
	output: PredictOutput,
	model_id: String,
}

#[derive(Debug, serde::Serialize)]
struct TrueValueEvent {
	date: chrono::DateTime<chrono::Utc>,
	identifier: NumberOrString,
	model_id: String,
	true_value: NumberOrString,
}

#[derive(Debug, serde::Serialize, FromPyObject)]
#[serde(untagged)]
enum NumberOrString {
	Number(f64),
	String(String),
}

fn predict_input(py: Python) -> PyResult<PyObject> {
	let typing = py.import("typing")?;
	let py_dict = typing.getattr("Dict")?;
	let py_str = py.eval("str", None, None)?;
	let py_any = typing.getattr("Any")?;
	let predict_input = py_dict.get_item((py_str, py_any))?;
	Ok(predict_input.into())
}

fn predict_output(py: Python) -> PyResult<PyObject> {
	let typing = py.import("typing")?;
	let py_union = typing.getattr("Union")?;
	let predict_output = py_union.get_item((
		RegressionPredictOutput::type_object(py),
		BinaryClassificationPredictOutput::type_object(py),
		MulticlassClassificationPredictOutput::type_object(py),
	))?;
	Ok(predict_output.into())
}

fn feature_contribution_entry(py: Python) -> PyResult<PyObject> {
	let typing = py.import("typing")?;
	let py_union = typing.getattr("Union")?;
	let feature_contribution_entry = py_union.get_item((
		IdentityFeatureContribution::type_object(py),
		NormalizedFeatureContribution::type_object(py),
		OneHotEncodedFeatureContribution::type_object(py),
		BagOfWordsFeatureContribution::type_object(py),
		BagOfWordsCosineSimilarityFeatureContribution::type_object(py),
		WordEmbeddingFeatureContribution::type_object(py),
	))?;
	Ok(feature_contribution_entry.into())
}

fn ngram(py: Python) -> PyResult<PyObject> {
	let typing = py.import("typing")?;
	let py_union = typing.getattr("Union")?;
	let py_tuple = typing.getattr("Tuple")?;
	let py_str = py.eval("str", None, None)?;
	let tuple = py_tuple.get_item((py_str, py_str))?;
	let ngram = py_union.get_item((tuple, py_str))?;
	Ok(ngram.into())
}

macro_rules! repr {
	($ty:ty) => {
		#[pyproto]
		impl pyo3::PyObjectProtocol for $ty {
			fn __repr__(&self) -> PyResult<String> {
				Ok(format!("{:?}", self))
			}
		}
	};
}

repr!(RegressionPredictOutput);
repr!(BinaryClassificationPredictOutput);
repr!(MulticlassClassificationPredictOutput);

struct TangramError(anyhow::Error);

impl std::fmt::Display for TangramError {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		self.0.fmt(f)
	}
}

impl From<TangramError> for PyErr {
	fn from(error: TangramError) -> PyErr {
		PyErr::new::<pyo3::exceptions::PyTypeError, _>(error.to_string())
	}
}

#[derive(Clone, Debug)]
enum ColumnType {
	Number(NumberColumn),
	Enum(EnumColumn),
	Text(TextColumn),
}

#[derive(FromPyObject, Clone, Debug)]
struct NumberColumn {
	name: String,
}

#[derive(FromPyObject, Clone, Debug)]
struct EnumColumn {
	name: String,
	variants: Vec<String>,
}

#[derive(FromPyObject, Clone, Debug)]
struct TextColumn {
	name: String,
}

impl<'source> FromPyObject<'source> for ColumnType {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let ty: &str = ob.get_item("type")?.extract()?;
		let name: String = ob.get_item("name")?.extract()?;
		match ty {
			"number" => Ok(ColumnType::Number(NumberColumn { name })),
			"text" => Ok(ColumnType::Text(TextColumn { name })),
			"enum" => {
				let variants: Vec<String> = ob.get_item("variants")?.extract()?;
				Ok(ColumnType::Enum(EnumColumn { name, variants }))
			}
			&_ => Err(pyo3::exceptions::PyValueError::new_err(format!(
				"invalid variant type {}",
				ty,
			))),
		}
	}
}

impl Into<tangram_core::config::Column> for ColumnType {
	fn into(self) -> tangram_core::config::Column {
		match self {
			ColumnType::Number(column) => {
				tangram_core::config::Column::Number(tangram_core::config::NumberColumn {
					name: column.name,
				})
			}
			ColumnType::Enum(column) => {
				tangram_core::config::Column::Enum(tangram_core::config::EnumColumn {
					name: column.name,
					variants: column.variants,
				})
			}
			ColumnType::Text(column) => {
				tangram_core::config::Column::Text(tangram_core::config::TextColumn {
					name: column.name,
				})
			}
		}
	}
}

#[derive(Clone)]
pub struct FromArrowOptions<'a> {
	pub column_types: Option<BTreeMap<String, tangram_table::TableColumnType>>,
	pub infer_options: tangram_table::InferOptions,
	pub invalid_values: &'a [&'a str],
}

impl<'a> Default for FromArrowOptions<'a> {
	fn default() -> Self {
		Self {
			column_types: Default::default(),
			infer_options: Default::default(),
			invalid_values: Default::default(),
		}
	}
}

#[pyfunction]
fn train_inner(
	arrow_arrays: Vec<(String, &PyAny)>,
	target: String,
	column_types: Option<Vec<ColumnType>>,
	shuffle_enabled: Option<bool>,
	shuffle_seed: Option<u64>,
	test_fraction: Option<f32>,
	comparison_fraction: Option<f32>,
	autogrid: Option<AutoGridOptions>,
	grid: Option<Vec<GridItem>>,
	comparison_metric: Option<ComparisonMetric>,
) -> PyResult<Model> {
	// Construct the dataset
	let column_names = arrow_arrays
		.iter()
		.map(|(name, _)| name.to_owned())
		.collect::<Vec<_>>();
	let arrays = arrow_arrays
		.into_iter()
		.map(|(_, array)| array_to_rust(array).unwrap())
		.collect::<Vec<_>>();
	let dataset = tangram_core::train::TrainingDataSource::ArrowArrays {
		arrays,
		column_names,
	};

	// Construct the config options
	let config = make_config(
		column_types,
		shuffle_enabled,
		shuffle_seed,
		test_fraction,
		comparison_fraction,
		autogrid,
		grid,
		comparison_metric,
	);

	let mut trainer =
		tangram_core::train::Trainer::prepare(dataset, &target, config, &mut |_| {}).unwrap();
	let train_grid_item_outputs = trainer.train_grid(None, &mut |_| {}).unwrap();
	let model = trainer
		.test_and_assemble_model(train_grid_item_outputs, &mut |_| {})
		.unwrap();
	// TODO set the url!
	let tangram_url = "https://app.tangram.dev".to_owned();
	let tangram_url = tangram_url.parse().unwrap();

	let model = Model {
		model: model.clone().into(),
		log_queue: Vec::new(),
		tangram_url,
		core_model: CoreModel::Model(model),
	};
	Ok(model)
}

fn make_config(
	column_types: Option<Vec<ColumnType>>,
	shuffle_enabled: Option<bool>,
	shuffle_seed: Option<u64>,
	test_fraction: Option<f32>,
	comparison_fraction: Option<f32>,
	autogrid: Option<AutoGridOptions>,
	grid: Option<Vec<GridItem>>,
	comparison_metric: Option<ComparisonMetric>,
) -> tangram_core::config::Config {
	let column_types: Option<Vec<tangram_core::config::Column>> =
		column_types.map(|column_config| {
			column_config
				.into_iter()
				.map(|column| column.into())
				.collect()
		});
	let mut dataset_config = tangram_core::config::Dataset::default();
	dataset_config.columns = column_types.unwrap_or_default();
	if let Some(shuffle_seed) = shuffle_seed {
		dataset_config.shuffle.seed = shuffle_seed
	}
	if let Some(shuffle_enabled) = shuffle_enabled {
		dataset_config.shuffle.enable = shuffle_enabled
	}
	if let Some(test_fraction) = test_fraction {
		dataset_config.test_fraction = test_fraction
	}
	if let Some(comparison_fraction) = comparison_fraction {
		dataset_config.comparison_fraction = comparison_fraction
	}
	tangram_core::config::Config {
		dataset: dataset_config,
		features: Default::default(),
		train: tangram_core::config::Train {
			autogrid: autogrid.map(Into::into),
			grid: grid.map(|grid| grid.into_iter().map(Into::into).collect::<Vec<_>>()),
			comparison_metric: comparison_metric.map(Into::into),
		},
	}
}

#[derive(Debug, FromPyObject)]
struct AutoGridOptions {
	model_types: Vec<ModelType>,
}

impl Into<tangram_core::config::AutoGridOptions> for AutoGridOptions {
	fn into(self) -> tangram_core::config::AutoGridOptions {
		tangram_core::config::AutoGridOptions {
			model_types: Some(
				self.model_types
					.into_iter()
					.map(|item| item.into())
					.collect(),
			),
		}
	}
}

#[derive(Debug)]
enum ModelType {
	Linear,
	Tree,
}

impl<'source> FromPyObject<'source> for ModelType {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let ty: &str = ob.get_item("type")?.extract()?;
		match ty {
			"linear" => Ok(ModelType::Linear),
			"tree" => Ok(ModelType::Tree),
			&_ => Err(pyo3::exceptions::PyValueError::new_err(format!(
				"invalid variant type {}",
				ty,
			))),
		}
	}
}

impl Into<tangram_core::config::ModelType> for ModelType {
	fn into(self) -> tangram_core::config::ModelType {
		match self {
			ModelType::Linear => tangram_core::config::ModelType::Linear,
			ModelType::Tree => tangram_core::config::ModelType::Tree,
		}
	}
}

#[derive(Debug)]
enum GridItem {
	Tree(TreeGridItem),
	Linear(LinearGridItem),
}

impl<'source> FromPyObject<'source> for GridItem {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let ty: &str = ob.get_item("type")?.extract()?;
		match ty {
			"linear" => Ok(GridItem::Linear(ob.extract()?)),
			"tree" => Ok(GridItem::Tree(ob.extract()?)),
			&_ => Err(pyo3::exceptions::PyValueError::new_err(format!(
				"invalid variant type {}",
				ty,
			))),
		}
	}
}

impl Into<tangram_core::config::GridItem> for GridItem {
	fn into(self) -> tangram_core::config::GridItem {
		match self {
			GridItem::Tree(item) => tangram_core::config::GridItem::Tree(item.into()),
			GridItem::Linear(item) => tangram_core::config::GridItem::Linear(item.into()),
		}
	}
}

#[derive(Default, Debug)]
struct LinearGridItem {
	early_stopping_options: Option<EarlyStoppingOptions>,
	l2_regularization: Option<f32>,
	learning_rate: Option<f32>,
	max_epochs: Option<u64>,
	n_examples_per_batch: Option<u64>,
}

impl<'source> FromPyObject<'source> for LinearGridItem {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let mut linear_grid_item: LinearGridItem = Default::default();
		if let Ok(item) = ob.get_item("early_stopping_options") {
			linear_grid_item.early_stopping_options = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("l2_regularization") {
			linear_grid_item.l2_regularization = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("learning_rate") {
			linear_grid_item.learning_rate = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_epochs") {
			linear_grid_item.max_epochs = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("n_examples_per_batch") {
			linear_grid_item.n_examples_per_batch = Some(item.extract()?);
		}
		Ok(linear_grid_item)
	}
}

impl Into<tangram_core::config::LinearGridItem> for LinearGridItem {
	fn into(self) -> tangram_core::config::LinearGridItem {
		tangram_core::config::LinearGridItem {
			early_stopping_options: self.early_stopping_options.map(Into::into),
			l2_regularization: self.l2_regularization.map(Into::into),
			learning_rate: self.learning_rate,
			max_epochs: self.max_epochs,
			n_examples_per_batch: self.n_examples_per_batch.map(Into::into),
		}
	}
}

#[derive(Default, Debug)]
struct TreeGridItem {
	binned_features_layout: Option<BinnedFeaturesLayout>,
	early_stopping_options: Option<EarlyStoppingOptions>,
	l2_regularization_for_continuous_splits: Option<f32>,
	l2_regularization_for_discrete_splits: Option<f32>,
	learning_rate: Option<f32>,
	max_depth: Option<u64>,
	max_examples_for_computing_bin_thresholds: Option<u64>,
	max_leaf_nodes: Option<u64>,
	max_rounds: Option<u64>,
	max_valid_bins_for_number_features: Option<u8>,
	min_examples_per_node: Option<u64>,
	min_gain_to_split: Option<f32>,
	min_sum_hessians_per_node: Option<f32>,
	smoothing_factor_for_discrete_bin_sorting: Option<f32>,
}

impl<'source> FromPyObject<'source> for TreeGridItem {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let mut tree_grid_item: TreeGridItem = Default::default();
		if let Ok(item) = ob.get_item("binned_features_layout") {
			tree_grid_item.binned_features_layout = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("early_stopping_options") {
			tree_grid_item.early_stopping_options = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("l2_regularization_for_continuous_splits") {
			tree_grid_item.l2_regularization_for_continuous_splits = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("l2_regularization_for_discrete_splits") {
			tree_grid_item.l2_regularization_for_discrete_splits = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("learning_rate") {
			tree_grid_item.learning_rate = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_depth") {
			tree_grid_item.max_depth = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_examples_for_computing_bin_thresholds") {
			tree_grid_item.max_examples_for_computing_bin_thresholds = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_leaf_nodes") {
			tree_grid_item.max_leaf_nodes = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_rounds") {
			tree_grid_item.max_rounds = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("max_valid_bins_for_number_features") {
			tree_grid_item.max_valid_bins_for_number_features = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("min_examples_per_node") {
			tree_grid_item.min_examples_per_node = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("min_gain_to_split") {
			tree_grid_item.min_gain_to_split = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("min_sum_hessians_per_node") {
			tree_grid_item.min_sum_hessians_per_node = Some(item.extract()?);
		}
		if let Ok(item) = ob.get_item("smoothing_factor_for_discrete_bin_sorting") {
			tree_grid_item.smoothing_factor_for_discrete_bin_sorting = Some(item.extract()?);
		}
		Ok(tree_grid_item)
	}
}

impl Into<tangram_core::config::TreeGridItem> for TreeGridItem {
	fn into(self) -> tangram_core::config::TreeGridItem {
		tangram_core::config::TreeGridItem {
			binned_features_layout: self.binned_features_layout.map(Into::into),
			early_stopping_options: self.early_stopping_options.map(Into::into),
			l2_regularization_for_continuous_splits: self.l2_regularization_for_continuous_splits,
			l2_regularization_for_discrete_splits: self.l2_regularization_for_discrete_splits,
			learning_rate: self.learning_rate,
			max_depth: self.max_depth,
			max_examples_for_computing_bin_thresholds: self
				.max_examples_for_computing_bin_thresholds,
			max_leaf_nodes: self.max_leaf_nodes,
			max_rounds: self.max_rounds,
			max_valid_bins_for_number_features: self.max_valid_bins_for_number_features,
			min_examples_per_node: self.min_examples_per_node,
			min_gain_to_split: self.min_gain_to_split,
			min_sum_hessians_per_node: self.min_sum_hessians_per_node,
			smoothing_factor_for_discrete_bin_sorting: self
				.smoothing_factor_for_discrete_bin_sorting,
		}
	}
}

#[derive(Debug)]
enum BinnedFeaturesLayout {
	RowMajor,
	ColumnMajor,
}

impl<'source> FromPyObject<'source> for BinnedFeaturesLayout {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let ty: &str = ob.extract()?;
		match ty {
			"row_major" => Ok(BinnedFeaturesLayout::RowMajor),
			"column_major" => Ok(BinnedFeaturesLayout::ColumnMajor),
			&_ => Err(pyo3::exceptions::PyValueError::new_err(format!(
				"invalid variant type {}",
				ty,
			))),
		}
	}
}

impl Into<tangram_core::config::BinnedFeaturesLayout> for BinnedFeaturesLayout {
	fn into(self) -> tangram_core::config::BinnedFeaturesLayout {
		match self {
			BinnedFeaturesLayout::RowMajor => tangram_core::config::BinnedFeaturesLayout::RowMajor,
			BinnedFeaturesLayout::ColumnMajor => {
				tangram_core::config::BinnedFeaturesLayout::ColumnMajor
			}
		}
	}
}

#[derive(FromPyObject, Debug)]
struct EarlyStoppingOptions {
	early_stopping_fraction: f32,
	n_rounds_without_improvement_to_stop: usize,
	min_decrease_in_loss_for_significant_change: f32,
}

impl Into<tangram_core::config::EarlyStoppingOptions> for EarlyStoppingOptions {
	fn into(self) -> tangram_core::config::EarlyStoppingOptions {
		tangram_core::config::EarlyStoppingOptions {
			early_stopping_fraction: self.early_stopping_fraction,
			n_rounds_without_improvement_to_stop: self.n_rounds_without_improvement_to_stop,
			min_decrease_in_loss_for_significant_change: self
				.min_decrease_in_loss_for_significant_change,
		}
	}
}

#[derive(Debug)]
enum ComparisonMetric {
	Mae,
	Mse,
	Rmse,
	R2,
	Accuracy,
	Auc,
	F1,
}

impl<'source> FromPyObject<'source> for ComparisonMetric {
	fn extract(ob: &'source PyAny) -> PyResult<Self> {
		let ty: &str = ob.extract()?;
		match ty {
			"mae" => Ok(ComparisonMetric::Mae),
			"mse" => Ok(ComparisonMetric::Mse),
			"rmse" => Ok(ComparisonMetric::Rmse),
			"r2" => Ok(ComparisonMetric::R2),
			"accuracy" => Ok(ComparisonMetric::Accuracy),
			"auc" => Ok(ComparisonMetric::Auc),
			"f1" => Ok(ComparisonMetric::F1),
			&_ => Err(pyo3::exceptions::PyValueError::new_err(format!(
				"invalid variant type {}",
				ty,
			))),
		}
	}
}

impl Into<tangram_core::config::ComparisonMetric> for ComparisonMetric {
	fn into(self) -> tangram_core::config::ComparisonMetric {
		match self {
			ComparisonMetric::Mae => tangram_core::config::ComparisonMetric::Mae,
			ComparisonMetric::Mse => tangram_core::config::ComparisonMetric::Mse,
			ComparisonMetric::Rmse => tangram_core::config::ComparisonMetric::Rmse,
			ComparisonMetric::R2 => tangram_core::config::ComparisonMetric::R2,
			ComparisonMetric::Accuracy => tangram_core::config::ComparisonMetric::Accuracy,
			ComparisonMetric::Auc => tangram_core::config::ComparisonMetric::Auc,
			ComparisonMetric::F1 => tangram_core::config::ComparisonMetric::F1,
		}
	}
}

pub fn array_to_rust(obj: &PyAny) -> PyResult<ArrayRef> {
	// https://github.com/jorgecarleitao/arrow2/blob/aee543eea6fc6bc9d7b79234d6b8304a84d95fd5/arrow-pyarrow-integration-testing/src/lib.rs
	let array = Box::new(ffi::Ffi_ArrowArray::empty());
	let schema = Box::new(ffi::Ffi_ArrowSchema::empty());

	let array_ptr = &*array as *const ffi::Ffi_ArrowArray;
	let schema_ptr = &*schema as *const ffi::Ffi_ArrowSchema;

	obj.call_method1(
		"_export_to_c",
		(array_ptr as Py_uintptr_t, schema_ptr as Py_uintptr_t),
	)?;

	unsafe {
		let field = ffi::import_field_from_c(schema.as_ref()).unwrap();
		let array = ffi::import_array_from_c(array, &field).unwrap();
		Ok(array.into())
	}
}
