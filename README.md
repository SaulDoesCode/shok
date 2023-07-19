Shok is a monolithic social server written in rust.

## Features

- blogging with full text search
- sse based chat, economy features, redis like commands for variables scoped to your account that can be watched
- authentication and token based authorization on some routes
- file uploading and web server features like route rewriting and fuzzy matching on top level static files
- social features: timelines, reposts, likes, follows

## How to get it running

- install rust
- make a ./secrets with cert.pem and priv.pem, the admin password will be generated there
- cargo run --release
- go to <https://localhost> (it binds to 443 by default, still going to add command line args)
- use auth on the site and make an account the moniker admin if it is untaken will become the admin account

# HTTP API Documentation

Below is the documentation for the routes provided by the Rust code:

## Healthcheck

- **Path:** /healthcheck
- **Method:** GET
- **Description:** Endpoint to check the health status of the service.
- **Handler Function:** `health`

---

## Authentication

- **Path:** /auth
- **Methods:** POST, DELETE
- **Description:** Endpoint for user authentication and auto unauthentication.
- **Rate Limiter:** `auth_limiter` (Applied to POST method)
- **Handler Function (POST):** `auth_handler`
- **Handler Function (DELETE):** `auto_unauth`

---

## Unauthenticated Access

- **Path:** /unauth
- **Method:** GET
- **Description:** Endpoint for unauthenticated access.
- **Handler Function:** `auto_unauth`

---

## API

- **Path:** /api
- **Rate Limiter:** `api_limiter`
- **Description:** Main API endpoint for various commands and operations.
- **Middlewares:** Gzip Compression, Caching Headers (applied to all sub-paths)

### Command API

- **Path:** /api/cmd
- **Method:** POST
- **Description:** Endpoint to execute a command.
- **Handler Function:** `cmd_request`

### Moniker Lookup

- **Path:** /api/moniker-lookup/<id>
- **Method:** GET
- **Description:** Endpoint to lookup a moniker.
- **Handler Function:** `moniker_lookup`

### Search API

- **Path:** /api/search
- **Method:** POST
- **Description:** Endpoint for searching.
- **Middlewares:** Gzip Compression (enabled with minimum size), Caching Headers
- **Handler Function:** `search_api`

### Access API

- **Path:** /api/access/<ts>
- **Method:** GET
- **Description:** Endpoint to access the service with a specific timestamp.
- **Middlewares:** Gzip Compression (enabled with minimum size)
- **Handler Function:** `writ_access_purchase_gateway_api`

### Chat API

- **Path:** /api/chat
- **Method:** GET, POST
- **Description:** Endpoint for chat operations.
- **Middlewares:** Gzip Compression (enabled with minimum size)
- **Handler Function (GET):** `account_connected`
- **Handler Function (POST):** `chat_send`

### Search API by Timestamp

- **Path:** /api/search/<ts>
- **Method:** DELETE
- **Description:** Endpoint for deleting a search result with a specific timestamp.
- **Handler Function:** `search_api`

### Permission API

- **Path:** /api/perms
- **Method:** POST
- **Description:** Endpoint to modify permission schema.
- **Handler Function:** `modify_perm_schema`

### Make Tokens

- **Path:** /api/make-tokens
- **Method:** POST
- **Description:** Endpoint to make tokens.
- **Handler Function:** `make_token_request`

### Upload

- **Path:** /api/upload
- **Method:** POST
- **Description:** Endpoint for uploading static files.
- **Handler Function:** `upsert_static_file`

### List Uploads

- **Path:** /api/uploads
- **Method:** GET
- **Description:** Endpoint to list uploaded files.
- **Handler Function:** `list_uploads`

### Transference API

- **Path:** /api/tf
- **Methods:** POST, GET
- **Description:** Endpoint for data transference.
- **Handler Function (POST):** `transference_api`
- **Handler Function (GET):** `transference_api`

### Scoped Variable Store API

- **Path:** /api/svs/<moniker>
- **Method:** GET
- **Description:** Endpoint for scoped variable store operations.
- **Handler Function:** `scoped_variable_store_api`

### Resource API

- **Path:** /api/resource/<hash>
- **Method:** POST, GET
- **Description:** Endpoint for resource operations.
- **Handler Function (POST):** `resource_api`
- **Handler Function (GET):** `resource_api`

### Writ Likes API

- **Path:** /api/writ-likes/<ts>
- **Method:** GET
- **Description:** Endpoint to view likes for a specific Writ.
- **Handler Function:** `see_writ_likes`

### Account API

- **Path:** /api/<op>/<id>
- **Method:** POST, GET, DELETE
- **Description:** Endpoint for account-related operations.
- **Handler Function:** `account_api`

# Account API Documentation

The Account API provides various endpoints for managing user accounts and interactions with other users. Below are the available routes and their corresponding functionalities:

## Get Account Information

- **Path:** /api/\<op\>/\<id\>
- **Method:** GET
- **Description:** Endpoint for retrieving account information and performing various account-related operations.
- **Parameters:**
  - \<op\>: The operation to perform. Possible values: "follows", "following", "follow", "unfollow", "like", "unlike", "likes", "repost", "unrepost".
  - \<id\>: The ID of the target account.
- **Handlers:**
  - "follows": Get a list of accounts followed by the target account.
  - "following": Get a list of accounts following the target account.
  - "follow": Follow another account specified by the "other" parameter.
  - "unfollow": Unfollow another account specified by the "other" parameter.
  - "like": Like a status on a writ for another account specified by the "other" parameter.
  - "unlike": Remove a like from a status on a writ for another account specified by the "other" parameter.
  - "likes": Check if the target account likes a status on a writ owned by another account specified by the "other" parameter.
  - "repost": Repost a status from another account specified by the "other" parameter.
  - "unrepost": Remove a repost of a status from another account specified by the "other" parameter.
- **Response:**
  - A JSON object with the operation status and relevant data.

## Change Account Password

- **Path:** /api/\<op\>/\<id\>
- **Method:** POST
- **Description:** Endpoint for changing the account password.
- **Parameters:**
  - \<op\>: The operation to perform. Possible value: "change-password".
  - \<id\>: The ID of the target account.
- **Request Body:** A JSON object with the new password.
- **Response:**
  - A JSON object with the operation status and a message indicating whether the password change was successful.

## Delete Account

- **Path:** /api/\<op\>/\<id\>
- **Method:** DELETE
- **Description:** Endpoint for deleting the target account.
- **Parameters:**
  - \<op\>: The operation to perform. Possible values: "delete", "delete-all-writs".
  - \<id\>: The ID of the target account.
- **Handlers:**
  - "delete": Delete the target account.
  - "delete-all-writs": Delete all writs associated with the target account.
- **Response:**
  - A JSON object with the operation status and a message indicating whether the account deletion was successful.

## Error Responses

- If the provided method is not supported or the route does not match any of the defined paths, the API will respond with an error message indicating "no such method".

- If the provided request body for changing the password is not valid, the API will respond with an error message indicating "failed to parse password".

- If any operation fails due to an error, the API will respond with an error message describing the specific failure.

# Chat API Documentation

The Chat API provides endpoints for sending and handling chat messages. Below are the available routes and their corresponding functionalities:

## Send Chat Message

- **Handler:** `chat_send`
- **Path:** N/A
- **Method:** POST
- **Description:** Endpoint for sending chat messages.
- **Request Body:** A UTF-8 encoded string containing the chat message.
- **Response:**
  - If the message is successfully sent, the API will respond with a JSON object indicating the status as "ok".
  - If any errors occur during the process, the API will respond with an appropriate error message.

### Message Format

- The chat messages can be sent as simple text messages or in the form of commands.

### Simple Text Message

- If the message does not start with "@" or "/", it will be treated as a regular chat message and will be broadcasted to all users in the chat.

### Commands

- Commands are special chat messages that start with a "/" and are used to trigger specific actions.
- Available Commands:
  - `/help`: Display a list of available commands and their usage.
  - `/transfer <to> <amount> ?when`: Transfer a specified amount to another user with an optional timestamp "when" (if not provided, the current timestamp will be used).
  - `/whois <moniker>`: Retrieve information about a user identified by their "moniker".
  - `/follow <moniker>`: Follow an account identified by their "moniker".
  - `/unfollow <moniker>`: Unfollow an account identified by their "moniker".
  - `/balance` or `/b`: Check the account balance of the current user.
  - `/balance <amount>`: For administrators, add a specified amount to the account balance. u64, no negative amounts
  - `/broadcast <message>` or `/bc <message>`: Broadcast a message to all users in the chat.
  - `/msg <to> <message>` or `/m <to> <message>`: Send a private message to another user identified by their user ID or "moniker".
  - `/set <key> <value>`: Set a key-value pair in the user's scoped variable store.
  - `/get <key>`: Get the value of a key from the user's scoped variable store.
  - `/rm <key>`: Remove a key-value pair from the user's scoped variable store.
  - `/watch <key>`: Add a watcher for changes to a specific key in the user's scoped variable store.
  - `/unwatch <key>`: Remove a watcher for a specific key in the user's scoped variable store.
  - `/collection add <name> <variable> <value> ...`: Add variables to a named collection in the user's scoped variable store.
  - `/collection rm <name> <variable> ...`: Remove variables from a named collection in the user's scoped variable store.
  - `/collection get <name>`: Get the contents of a named collection in the user's scoped variable store.
  - `/collection has <name> <variable> ...`: Check if a named collection in the user's scoped variable store contains specific variables.
  - `/collection <name>`: Get the contents of a named collection in the user's scoped variable store.
  - `/set-tags <moniker> <tag> ...`: Set tags for a user identified by their "moniker".
  - `/get-tags <moniker>`: Get the tags for a user identified by their "moniker".
  - `/unset-tags <moniker> <tag> ...`: Remove tags from a user identified by their "moniker".
  - `/vars-with-tags <tag> ...`: Get variables from the scoped variable store of the current user that have specified tags.
  - `/server-timestamp`: Get the current server's time as a timestamp number
  - `/cmd command args args args`: Run a command on the server and get the output if you're the admin

### Error Responses

- If the user is not logged in (session check fails), the API will respond with an error message indicating "not logged in".
- If the message length exceeds the maximum limit (2048 characters), the API will respond with an error message indicating "message too long".
- If any other errors occur during message parsing or processing, the API will respond with an appropriate error message.

It is important to ensure that appropriate authentication and authorization mechanisms are implemented to protect sensitive chat functionalities from unauthorized access. Additionally, include further details about specific error codes and any additional requirements for each command in the actual implementation for a more comprehensive API documentation.

# Search API Documentation

The Search API provides endpoints for searching, adding, updating, and deleting items in the search index. Below are the available routes and their corresponding functionalities:

## Search for Items

- **Method:** GET
- **Description:** Endpoint for searching public searchable items based on a query.
- **Query Parameters:**
  - `q`: The search query string.
  - `p` (optional): The page number of the search results (default is 0).
  - `k` (optional): The kind of items to search for (if specified).
- **Response:**
  - If the search is successful, the API will respond with a JSON object containing an array of search results ("writs") and their corresponding owners' monikers.
  - If the query parameter "q" is missing, the API will respond with an error message indicating "no query provided".
  - If any errors occur during the search process, the API will respond with an appropriate error message.

## Search for Items (POST)

- **Method:** POST
- **Description:** Endpoint for searching public searchable items based on a JSON request body.
- **Request Body:** A JSON object containing the following fields:
  - `query`: The search query string.
  - `limit`: The maximum number of results to return.
  - `page`: The page number of the search results.
  - `kind` (optional): The kind of items to search for (if specified).
- **Response:**
  - If the search is successful, the API will respond with a JSON object containing an array of search results ("writs").
  - If any errors occur during the search process, the API will respond with an appropriate error message.

## Add or Update Item

- **Method:** PUT
- **Description:** Endpoint for adding or updating an item in the search index.
- **Request Body:** A JSON object representing the item to be added or updated. The object should have the following fields:
  - `ts` (optional): The timestamp of the item. If not provided, the current timestamp will be used.
  - `public`: A boolean indicating whether the item is public or not.
  - `owner`: The owner of the item.
  - `title`: The title of the item.
  - `kind`: The kind of item.
  - `content`: The content of the item.
  - `state`: The state of the item.
  - `price`: The price of the item.
  - `sell_price`: The selling price of the item.
  - `tags`: An array of tags associated with the item.
- **Response:**
  - If the add/update operation is successful, the API will respond with a JSON object indicating "ok: true".
  - If any errors occur during the add/update process, the API will respond with an appropriate error message.

## Remove Item

- **Method:** DELETE
- **Description:** Endpoint for removing an item from the search index.
- **URL Parameter:**
  - `ts`: The timestamp of the item to be removed.
- **Response:**
  - If the item is successfully removed, the API will respond with a JSON object containing "ok: true" and additional information about the operation.
  - If the "ts" parameter is missing or invalid, the API will respond with an error message indicating "failed to remove from index, invalid timestamp param".
  - If the item is not found in the index, the API will respond with an error message indicating "failed to remove from index, maybe it doesn't exist".
  - If the request is not authorized to remove the item (either the owner is different or not an admin), the API will respond with an error message indicating "not authorized to delete posts from the index without the right credentials".

## Update Item (PATCH)

- **Method:** PATCH
- **Description:** Endpoint for updating an item in the search index.
- **Request Body:** A JSON object representing the updated item. The object should have the following fields:
  - `ts` (optional): The timestamp of the item. If not provided, the current timestamp will be used.
  - `public`: A boolean indicating whether the item is public or not.
  - `owner`: The owner of the item.
  - `title`: The title of the item. (optional)
  - `kind`: The kind of item.
  - `content`: The content of the item.
  - `state`: The state of the item. (optional)
  - `price`: The price of the item. (optional)
  - `sell_price`: The selling price of the item. (optional)
  - `tags`: An array of tags associated with the item.
- **Response:**
  - If the update operation is successful, the API will respond with a JSON object indicating "ok: true".
  - If any errors occur during the update process, the API will respond with an appropriate error message.

### Error Responses

- If the request method is not allowed, the API will respond with an error message indicating "method not allowed".
- If any errors occur during the processing of requests (e.g., parsing errors, bad request body, etc.), the API will respond with an error message providing additional information about the error.

It is essential to ensure that proper authentication and authorization mechanisms are implemented to protect sensitive search functionalities from unauthorized access. Additionally, include further details about specific error codes and any additional requirements for each endpoint in the actual implementation for a more comprehensive API documentation.

# CMDRequest API Documentation

The CMDRequest API allows users with administrator privileges to execute commands and interact with the system through HTTP requests. Below are the available endpoints and their corresponding functionalities:

## Execute Command (POST)

- **Endpoint:** `/cmd_request`
- **Description:** Execute a command provided in the request body and return the command's output.
- **Required Privileges:** Administrator (admin)
- **Request Body:** A JSON object representing the command to be executed. The object should have the following fields:
  - `cmd`: The command to be executed.
  - `cd` (optional): The current directory path from which to run the command. If not provided, the command will run in the current working directory.
  - `args`: An array of arguments to be passed to the command.
  - `stream` (optional): A boolean indicating whether to stream the command's output as it runs. If `true`, the API will continuously stream the output to the client. If not provided or `false`, the output will be returned in the response after the command execution is complete.
  - `when` (optional): A timestamp indicating the scheduled execution time for the command. If provided and the timestamp is in the past, the command will be saved for future execution.
  - `again` (optional): A timestamp indicating a scheduled periodic execution of the command.
- **Response:**
  - If the command is successfully executed and the output is not being streamed (`stream` is not provided or `false`), the API will respond with a JSON object containing the command's output and any error messages from stderr.
  - If the command is scheduled for later execution (`when` is provided and the timestamp is in the future), the API will respond with a JSON object indicating that the command has been saved for later execution.
  - If the `stream` parameter is set to `true`, the API will continuously stream the command's output as lines of text in plain text format.

### Examples

#### Request

```json
{
  "cmd": "ls",
  "args": ["-l"],
  "stream": true
}
```

#### Response (Streaming)

```
total 56
drwxr-xr-x  2 user user 4096 Sep 23 14:05 bin
drwxr-xr-x  2 user user 4096 Sep 23 14:05 lib
drwxr-xr-x  2 user user 4096 Sep 23 14:05 usr
...
```

#### Request

```json
{
  "cmd": "echo",
  "args": ["Hello, World!"]
}
```

#### Response

```json
{
  "output": "Hello, World!\n",
  "err": ""
}
```

### Error Responses

- If the user does not have administrator privileges, the API will respond with an error message indicating "cmd_request: not admin" (unauthorized).
- If the request body is not a valid `CMDRequest` object, the API will respond with an error message indicating "failed to run command, bad CMDRequest".
- If there are any errors during the command execution process, the API will respond with an appropriate error message providing additional details about the error.

It is essential to ensure that proper authentication and authorization mechanisms are implemented to protect the CMDRequest endpoint from unauthorized access. Additionally, consider implementing rate-limiting and input validation to prevent abuse or malicious commands. The provided examples are illustrative and may not cover all possible use cases; consider adding more thorough examples and edge case handling for the actual implementation.
