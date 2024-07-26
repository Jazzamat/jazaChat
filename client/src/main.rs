use std::{io::{self, stdin, stdout, Read, Write}, net::TcpStream, str::FromStr};

fn main() -> io::Result<()>{
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


fn cli(username: String) {
    let stdin = stdin();
    let mut chat = String::new();
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
                get_chat();
            }
            _ => {}
        }

        // decorate and send message to server
        let buf = add_username(buf, &username);
        let buf = buf.as_str();
        let mut stream = TcpStream::connect("0.0.0.0:8080").unwrap();
        stream.write(buf.as_bytes()).unwrap();
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

fn get_chat() -> String {

    // just send over a zero byte to trigger the server response
    let mut get_command = String::new();
    get_command.push_str("$get\n");
    let mut stream = TcpStream::connect("0.0.0.0:8080").unwrap();
    stream.write(get_command.as_bytes()).unwrap();

    // reveice the chat from server
    let mut received = [0;10000];
    stream.read(&mut received).unwrap();

    let received = String::from_utf8_lossy(&received);
    let received = received.trim_matches(char::from(0));
    let value = String::from_str(received);
    println!("{value:#?}");
    stdout().flush().unwrap();
    value.unwrap()
}


