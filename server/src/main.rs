use std::{
    io::{self, stdout, Read, Write},
    net::{TcpListener, TcpStream}, thread,
};


fn main() -> io::Result<()> {
    server()
}

fn server() -> io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").unwrap();
    println!("This is the jazaServer. Listening on port 8080");
    let mut chat = String::new();
    let mut streams: Vec<TcpStream> = Vec::new();

    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        streams.push(stream.try_clone().unwrap());
        //thread::spawn( || {println!("sf");
            handle_stream(&mut stream, &mut chat, &mut streams)
        //});
    }
    Ok(())
}

fn handle_stream(stream: &mut TcpStream, chat: &mut String, streams: &mut Vec<TcpStream>) {
    println!("New stream lets go!: {stream:?}");
    loop {

        // need to figure out if the stream is no longer active so I can avoid an infinite loop

        let mut buf = [0; 100];
        stream.read(&mut buf).unwrap();
        println!("{buf:?}");
        if buf[0] == 0 {
            println!("Connection closed");
            streams.retain(|x| x.peer_addr().unwrap() != stream.peer_addr().unwrap());
            break;
        }

        let msg = String::from_utf8_lossy(&buf);
        let command = msg.clone().into_owned();
        let command = command.as_str().trim_matches(char::from(0));

        match command {
            _ if command == "$get\n" => {
                stdout().flush().unwrap();
                let mut buf = chat.as_bytes();
                stream.write(&mut buf).unwrap();
            }
            _ => {
                let msg = msg.into_owned();
                let msg = msg.trim_matches(char::from(0));
                chat.push_str(msg);
                println!("Chat: {chat:?}");
                println!("notifying streams...");
                notify_streams(streams, msg);
            }
        }
        println!("IM ABOUT TO FLUSH");
        stream.flush().unwrap();
        println!("I flushed...");
    }
}

fn notify_streams(streams: &mut Vec<TcpStream>, message: &str) {
    println!("For now imaging that this worked");
    for stream in streams {
        println!("Stream: {stream:?}");
        stream.write(message.as_bytes()).unwrap();
    }
}

fn write_to_clients() {
}
