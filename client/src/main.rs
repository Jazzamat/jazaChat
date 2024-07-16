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
        let mut buf = String::new();
        print!(">");
        stdout().flush().unwrap();
        stdin.read_line(&mut buf).unwrap();
        let buf = add_username(buf, &username);
        let buf = buf.as_str();
        let mut stream = TcpStream::connect("0.0.0.0:8080").unwrap();
        stream.write(buf.as_bytes()).unwrap();
        let mut received = [0;24];
        stream.read(&mut received).unwrap();
        let received = String::from_utf8_lossy(&received);
        chat.push_str(buf);
        println!("{chat:#?}")
    }
}


fn add_username(buf: String, username: &String) -> String { 
    let mut result = String::new();
    result.push('[');
    result.push_str(username);
    result.push(']');
    result.push(' ');
    result.push_str(username.as_str());
    result
}

