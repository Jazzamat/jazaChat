use std::{io::{self, Read, Write}, net::TcpListener};



fn server() -> io::Result<()>{

    let listener = TcpListener::bind("0.0.0.0:8080").unwrap();
    println!("This is the jazaServer. Listening on port 8080");

    let mut chat = String::new();

    for stream in listener.incoming() {
        let mut buf = [0;10];
        let mut stream = stream.unwrap();
        stream.read(&mut buf)?;
        let msg = String::from_utf8_lossy(&buf);
        let msg = msg.into_owned();

        chat.push_str(msg.as_str());


        println!("Chat: {chat:?}");
        let mut buf = chat.as_bytes();
        stream.write(&mut buf)?;
    }
    Ok(())
}
