use include::*;
use std;

mod views;

fn msleep(ms: u64) {
	sleep(Duration::from_millis(ms));
}

pub trait Ext<'a> {
	fn ext<T: typemap::Key>(&'a self) -> &'a T::Value;
	fn ins<T: typemap::Key>(&mut self, val: T::Value);
}

impl<'a, 'b> Ext<'a> for Request<'a, 'b> {
	fn ext<T: typemap::Key>(&'a self) -> &'a T::Value {
		self.extensions.get::<T>().unwrap()
	}
	fn ins<T: typemap::Key>(&mut self, val: T::Value) {
		self.extensions.insert::<T>(val);
	}
}

pub enum Reply {
	Html(String),
	Redirect(String),
}

pub fn enter() {

	// Define these routes, use 'elm' to access extras. How to add own extras?
	// Can still use Iron's after/before.
	// Defaults: Request + Database + RevRoute + Logger
	// Since a single route handles a 'series' of similar requests, (similar in origin)
	// We can reasonably assume that we have a "global surrounder" for that route.
	// router.surroundHtml(|html| {
	// 	html! {
	// head {
	// 	something
	// } body {
	// 	^PreEscaped(html)
	// 	footer here
	// }
	// }});
	//
	// I like this... anything else that the site needs? Ah, cookies!
	// Or even user login, given as an Option<User>
	// What about inserting more BeforeMiddleware after the Db connection?
	// I'd ideally want to let the user assemble middleware.
	// Req -> Logger -> Db -> Cookie -> User -> RevRoute -> Handler
	// Actually we already can! Just use link_before, since it appends!
	// What about customization of Db, Cookie, etc? Also customizing elm

	let router = hybrid! {

		get "/", myfun: (_, elm) => {
			msleep(1000);
			trace![elm.log, "Nice", "linkback" => elm.rev.kek];
			Reply::Html(views::index(&*elm.log))
		},

		get "/other/:test", kek: (req, elm) => {
			msleep(1000);
			trace![elm.log, "cool", "req" => format!("{:?}", req.ext::<Router>().find("test"))];
			Reply::Html("Hello World".to_owned())
		},

		get "/*", some: (req, elm) => {
			msleep(1000);
			warn![elm.log, "Unknown route", "req" => format!("{:?}", req)];
			Reply::Redirect(elm.rev.kek.into())
		},

	};

	let log = setup_logger(get_loglevel("SLOG_LEVEL"));
	let mainlog = log.new(o!["reqid" => "main"]);
	let worklog = log.new(o![]);

	defer!(trace![mainlog, "Clean exit"]);
	trace![mainlog, "Constructing middleware"];

	let mut chain = router;
	chain.link_before(Log::new(worklog));
	chain.link_before(Db);
	chain.link_around(ResponseTime);
	chain.link_after(Html);

	let mut mount = Mount::new();
	mount.mount("/", chain)
		.mount("/dl/", Static::new(Path::new("dl/")));

	trace![mainlog, "Starting server"];
	let _ = Iron::new(mount).http("localhost:3000").map_err(|x| {
		error![mainlog, "Unable to start server", "error" => format!("{:?}", x)];
	});
}

pub struct Html;
impl AfterMiddleware for Html {
	fn after(&self, req: &mut Request, mut res: Response) -> IronResult<Response> {
		trace![req.ext::<Log>(), "Setting MIME to html"];
		(Mime(TopLevel::Text, SubLevel::Html, vec![])).modify(&mut res);
		Ok(res)
	}
}

pub struct Log(Arc<Logger>, Mutex<u64>);
impl Log {
	fn new(log: Logger) -> Log {
		Log(Arc::new(log), Mutex::new(0))
	}
}

impl typemap::Key for Log {
	type Value = Arc<Logger>;
}
impl BeforeMiddleware for Log {
	fn before(&self, req: &mut Request) -> IronResult<()> {
		let reqid = {
			let mut count = self.1.lock().unwrap();
			*count = count.wrapping_add(1);
			*count
		};
		req.ins::<Log>(Arc::new(self.0.new(o!["reqid" => reqid])));
		trace![req.ext::<Log>(), "Beginning request"];
		Ok(())
	}
}

struct Head;
impl typemap::Key for Head {
	type Value = String;
}
impl BeforeMiddleware for Head {
	fn before(&self, req: &mut Request) -> IronResult<()> {
		let mut buffer = String::new();
		let _ = html! {
			buffer,
			head {
				meta charset="UTF-8" /
			}
		};
		req.ins::<Head>(buffer);
		Ok(())
	}
}

struct ResponseTime;
impl AroundMiddleware for ResponseTime {
	fn around(self, handler: Box<Handler>) -> Box<Handler> {
		Box::new(ResponseTimeHandler(handler))
	}
}

struct ResponseTimeHandler(Box<Handler>);
impl Handler for ResponseTimeHandler {
	fn handle(&self, req: &mut Request) -> IronResult<Response> {
		let begin = precise_time_ns();
		let response = self.0.handle(req);
		let delta = precise_time_ns() - begin;

		trace!(req.ext::<Log>(), "Request time",
			"ms" => delta / 1000 / 1000, "us" => delta / 1000 % 1000, "ns" => delta % 1000
		);

		response
	}
}

pub struct Db;
impl typemap::Key for Db {
	type Value = Rc<Connection>;
}
impl BeforeMiddleware for Db {
	fn before(&self, req: &mut Request) -> IronResult<()> {
		let path = "postgresql://postgres:abc@localhost/hybrida";
		debug![req.ext::<Log>(), "Connecting to database", "path" => path];
		let conn = Connection::connect(path, SslMode::None).map_err(|x| {
			crit![req.ext::<Log>(), "Unable to connec to db", "error" => format!("{:?}", x)];
		});
		if let Ok(conn) = conn {
			req.ins::<Db>(Rc::new(conn));
		}
		Ok(())
	}
}

fn get_loglevel(env: &str) -> Level {
	macro_rules! lvlc {
		($n:expr, $($i:ident),*) => {{
			match $n {
				$(
					stringify!($i) => Level::$i,
				)*
				_ => Level::Info,
			}
		}};
	}
	match env::var(env) {
		Ok(val) => lvlc![&val[..], Trace, Debug, Info, Warning, Error],
		Err(_) => Level::Info,
	}
}

fn setup_logger(level: Level) -> Logger {
	let automatic = o!["line" => {
			|rec: &RecordInfo| {
				rec.line()
			}
		}, "mod" => {
			|rec: &RecordInfo| {
				rec.module().to_owned()
			}
		}];

	let log;
	if stderr_isatty() {
		log = drain::filter_level(level, ::slog_term::async_stderr()).into_logger(automatic);
		trace!(log, "Using drain", "out" => "stderr",
			"stderr_isatty" => stderr_isatty(),
			"type" => "term");
	} else {
		log = drain::filter_level(level,
		                          drain::async_stream(std::io::stderr(), ::slog_json::new()))
			.into_logger(automatic);
		trace!(log, "Using drain", "out" => "stderr",
			"stderr_isatty" => stderr_isatty(),
			"type" => "json");
	}
	log
}
