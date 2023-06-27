import d from 'https://cdn.jsdelivr.net/gh/SaulDoesCode/kurshok/dist/domlib.js'
const {section, div, span, h1, header, article} = d.domfn
const {query} = d
await d.ready

const articles = div.articles({$: 'main > section.posts'})
const post = (title, ...contents) => article({
    $: articles
},
    header('hello world'),
    section.content(
        ...contents
    )
)

post('hello world', `
    first blog post
`)