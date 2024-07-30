use std::{fs, io::{self, stdin, stdout, Read, Write}, net::TcpStream, str::FromStr};

static SERVER_ADDRESS: &str = "0.0.0.0:8080";

fn main() -> io::Result<()>{
    renderAsciiTitle();
    let username = intro();
    cli(username);
    Ok(())
}

fn intro() -> String {
    println!("Welcome to jazaChat");
    print!("Username: ");
    stdout().flush().unwrap();
    let stdin = stdin();
    let mut buf = String::new();
    stdin.read_line(&mut buf).unwrap();
    let username = buf.trim_matches('\n');
    String::from_str(username).unwrap()
}

fn renderAsciiTitle() {
    let contents = fs::read_to_string("ascititle.txt").expect("could read find this file");
    println!("{contents}")
}

fn cli(username: String) {
    let stdin = stdin();
    let mut chat = String::new();

    let mut stream = TcpStream::connect(SERVER_ADDRESS).unwrap();
    loop {
        // get input from stdin
        let mut buf = String::new();
        print!(">");
        stdout().flush().unwrap();
        stdin.read_line(&mut buf).unwrap();

        // process users command
        let command = buf.as_str();
        match command {
            _ if command == "$get\n"  => {
                stdout().flush().unwrap();
                // get_chat(&mut stream);
            }
            _ => {
                // decorate and send message to server
                let buf = add_username(buf, &username);
                let buf = buf.as_str();

                match stream.write(buf.as_bytes()) {
                    Ok(response) => {}
                    Err(error) => {
                        println!("jazaChat: Error occured when writing:");
                        println!("{error:?}");
                    }
                }
                receive_from_stream(&mut stream);
            }
        }
        stream.flush().unwrap();
    }
}

fn add_username(buf: String, username: &String) -> String { 
    let mut result = String::new();
    result.push('[');
    result.push_str(username);
    result.push(']');
    result.push(' ');
    result.push_str(buf.as_str());
    result
}

fn get_chat(stream: &mut TcpStream) -> &mut TcpStream {
    // send over the $get command.
    let mut get_command = String::new();
    get_command.push_str("$get\n");
    stream.write(get_command.as_bytes()).unwrap();
    receive_from_stream(stream);
    stream
}

fn receive_from_stream(stream: &mut TcpStream) {
    // reveice the chat from server
    let mut received = [0;10000];
    stream.read(&mut received).unwrap();

    let received = String::from_utf8_lossy(&received);
    let received = received.trim_matches(char::from(0));
    let value = String::from_str(received).unwrap();
    render(received);
    stdout().flush().unwrap();
}


fn render(chat: &str) { // problably don't need this
    for c in chat.chars() {
        print!("{}", c)
    }
}
