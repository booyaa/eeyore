extern crate hyper;
extern crate hubcaps;
extern crate iron;
extern crate router;
extern crate inth_oauth2;
extern crate cookie;
extern crate oven;
extern crate handlebars_iron as hbs;
extern crate rustc_serialize;
extern crate params;
extern crate dotenv;
#[macro_use]
extern crate log;

use iron::prelude::*;
use iron::status;
use iron::headers::{ContentType, Location};
use iron::modifiers::Header;
use params::{Params, Value};
use oven::{RequestExt, ResponseExt};
use router::Router;
use inth_oauth2::provider::GitHub;
use inth_oauth2::token::Token;
use hbs::{HandlebarsEngine, Template, DirectorySource};
use rustc_serialize::json::{Json, ToJson};
use dotenv::dotenv;

use std::env;
use std::collections::BTreeMap;

const USER_AGENT: &'static str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

fn main() {
    dotenv().ok();

    let cookie_signing_key = env::var("SECRET")
                                 .expect("SECRET must be specified to sign cookies")
                                 .as_bytes()
                                 .to_vec();

    let mut router = Router::new();
    router.get("/", |request: &mut Request| {
        let repos_enabled = match request.get_cookie("datastore") {
            Some(data) => data.value.clone(),
            None => String::new(),
        };

        println!("GET / cookie datastore: {:?}", repos_enabled);

        Ok(Response::with((status::Ok,
                           Header(ContentType::html()),
                           "<html><body>home | <a href=/triage>triage</a> | <a \
                            href=/repos>repos</a> | stuff<br /><div><a href='/oauth'>Log in with \
                            Github</a></div></body></html>")))
    });

    router.get("/oauth", |_: &mut Request| {
        let oauth_client = github_client();
        let auth_uri = oauth_client.auth_uri(Some("write:repo_hook,public_repo"), None).unwrap();
        Ok(redirect_response(auth_uri.to_string()))
    });

    router.get("/callback", |request: &mut Request| {
        let params = request.get_ref::<Params>().unwrap();
        let code = match *params.get("code").unwrap() {
            params::Value::String(ref value) => value,
            _ => panic!("No oauth code found in request."),
        };

        let oauth_client = github_client();
        let bearer_token = oauth_client.request_token(&Default::default(), code.trim()).unwrap();

        let mut response = redirect_response(String::from("/repos"));
        response.set_cookie(cookie::Cookie::new(String::from("access_token"),
                                                String::from(bearer_token.access_token())));
        Ok(response)
    });

    router.get("/repos", |request: &mut Request| {
        let access_token = match request.get_cookie("access_token") {
            Some(token) => token.value.clone(),
            None => return not_logged_in(),
        };

        let repos_enabled = match request.get_cookie("datastore") {
            Some(data) => data.value.clone(),
            None => String::new(),
        };

        println!("GET /repos cookie datastore: {:?}", repos_enabled);

        let repos = authorized_repos(&access_token);
        let mut data: BTreeMap<String, Json> = BTreeMap::new();

        // big list! println!("repos: {:#?}", repos);
        // TODO: Merge authorized_repos w/ datastore cookie
        let repo_data = repos.into_iter()
                             .take(5) // TODO: paginaters gonna paginate
                             .filter(|x| x.full_name != repos_enabled) // I don't think this is desired behaviour. Maybe modify data's structure to include a state field?
                             .map(|r| {
                                 let mut d = BTreeMap::new();
                                 d.insert(String::from("full_name"), r.full_name.to_json());
                                 d
                             })
                             .collect::<Vec<_>>();

        // TODO: Save default state to datastore cookie
        println!("raw:{:#?}\njson:{:#?}", repo_data, repo_data.to_json());
        data.insert(String::from("repos"), repo_data.to_json());

        Ok(Response::with((status::Ok, Template::new("repos", data))))
    });

    router.post("/enablement", |request: &mut Request| {
        // sample response from form: {"repo": "booyaa/anchor"}
        let params = request.get_ref::<Params>().unwrap();

        let key = "repo";;
        let mut param_value = String::new();
        match params.get(key.into()) {
            Some(&Value::String(ref value)) => {
                println!("POST /enablement value: {}", value);
                param_value = format!("{}", value);
            }
            _ => {}
        }

        println!("POST /enablement params: {:?}", params);

        // store in cookie for now
        // TODO: display another form to capture who triagers are
        let mut response = Response::with((status::Ok,
                                           Header(ContentType::html()),
                                           "<html><body><div>Enabled! <a href='/repos'>Go \
                                            back</a></div></body></html>"));

        response.set_cookie(cookie::Cookie::new(String::from("datastore"), param_value));
        Ok(response)

    });

    router.get("/triage", |request: &mut Request| {
        // get issues?
        // allow you to select multiple issues
        // # | title | desc | labels | date (created/last mod)
        // Issue.title / Issue.body / Labels / created_at / updated_at
        let access_token = match request.get_cookie("access_token") {
            Some(token) => token.value.clone(),
            None => return not_logged_in(),
        };

        let issues = authorized_issues(&access_token);

        println!("GET /triage {:#?}", issues);
        Ok(Response::with((status::Ok,
                           Header(ContentType::html()),
                           "<html><body><a href=/>home</a> | triage | <a href=/repos>repos</a> | \
                            stuff<br /><div><a href='/oauth'>Log in with \
                            Github</a></div></body></html>")))
    });

    let mut chain = Chain::new(router);

    chain.link(oven::new(cookie_signing_key));

    let mut hbse = HandlebarsEngine::new();
    hbse.add(Box::new(DirectorySource::new("./src/templates/", ".hbs")));
    // load templates from all registered sources
    if let Err(r) = hbse.reload() {
        panic!("{:?}", r);
    }
    chain.link_after(hbse);
    println!("Server running at http://localhost:3000/");
    Iron::new(chain).http("localhost:3000").unwrap();
}

fn github_client() -> inth_oauth2::Client<GitHub> {
    inth_oauth2::Client::<GitHub>::new(env::var("CLIENT_ID")
                                           .expect("Github OAuth CLIENT_ID must be specified"),
                                       env::var("CLIENT_SECRET")
                                           .expect("Github OAuth CLIENT_SECRET must be specified"),
                                       env::var("REDIRECT_URI").ok())
}

fn authorized_repos(access_token: &str) -> Vec<hubcaps::rep::Repo> {
    let user_client = hyper::Client::new();
    let user_github = hubcaps::Github::new(USER_AGENT,
                                           &user_client,
                                           hubcaps::Credentials::Token(access_token.to_string()));
    let repos = user_github.repos().list(&Default::default()).unwrap();
    // TODO: filter to only return repositories on which the user has admin permissions
    // TODO: paginate to get all repos, not currently supported by hubcaps
    repos
}

fn authorized_issues(access_token: &str) -> std::vec::Vec<hubcaps::Issue> {
    let user_client = hyper::Client::new();
    let user_github = hubcaps::Github::new(USER_AGENT,
                                           &user_client,
                                           hubcaps::Credentials::Token(access_token.to_string()));
    let repository = user_github.repo("booyaa".to_string(), "hello-homu".to_string());
    let issues_dataset = repository.issues();
    let issues = issues_dataset.list(&Default::default()).unwrap();
    issues
}



fn not_logged_in() -> Result<Response, iron::error::IronError> {
    // TODO: add some indication that you've been redirected because you weren't
    // signed in and we needed you to be
    Ok(redirect_response(String::from("/")))
}

fn redirect_response(redirect_uri: String) -> Response {
    Response::with((status::Found,
                    Header(Location(redirect_uri.clone())),
                    format!("You are being <a href='{}'>redirected</a>.", redirect_uri)))
}
