use reqwest::blocking::Response;

pub(crate) fn is_response_valid(response: &Response) -> bool {
    if response.status().is_server_error() {
        println!("Got server error: {}", response.status());
        return true;
    } else if response.status().is_client_error() {
        println!("Got client error: {}", response.status());
        return true;
    } else if !response.status().is_success() {
        println!("Got communication error: {}", response.status());
        return true;
    }
    false
}